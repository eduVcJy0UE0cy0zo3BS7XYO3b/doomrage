use crate::preprocessor;
use crate::render::{PlotData, RenderBlock};
use anyhow::Result;
use scheme_rs::env::TopLevelEnvironment;
use scheme_rs::runtime::Runtime;
use scheme_rs::value::Value;
use std::collections::HashMap;

const RENDER_PRELUDE: &str = r#"
;; Sentinel for uncomputed values
(define <compute> "<compute>")
(define (compute? x) (and (string? x) (string=? x "<compute>")))

;; Stringify helper: propagates <compute>
(define (->str x)
  (cond
    ((compute? x) "<compute>")
    ((string? x) x)
    ((number? x) (number->string x))
    ((boolean? x) (if x "true" "false"))
    (else "?")))

;; Arithmetic that propagates <compute>
(define (orig+ . args) (apply + args))
(define (orig- . args) (apply - args))
(define (orig* . args) (apply * args))
(define (orig/ . args) (apply / args))

(define (safe+ . args) (if (exists compute? args) <compute> (apply orig+ args)))
(define (safe- . args) (if (exists compute? args) <compute> (apply orig- args)))
(define (safe* . args) (if (exists compute? args) <compute> (apply orig* args)))
(define (safe/ . args) (if (exists compute? args) <compute> (apply orig/ args)))
(define (safe-min . args) (if (exists compute? args) <compute> (apply min args)))
(define (safe-max . args) (if (exists compute? args) <compute> (apply max args)))
(define (safe-abs x) (if (compute? x) <compute> (abs x)))
(define (safe-sqrt x) (if (compute? x) <compute> (sqrt x)))

;; Render primitives
(define (text . args) (list 'render-text (apply string-append (map ->str args))))
(define (bold . args) (list 'render-bold (apply string-append (map ->str args))))
(define (italic . args) (list 'render-italic (apply string-append (map ->str args))))
(define (code str) (list 'render-code (->str str)))
(define (link url label) (list 'render-link url label))
(define (hr) (list 'render-hr))
(define (table headers rows) (list 'render-table (map ->str headers) (map (lambda (r) (map ->str r)) rows)))
(define (plot-line data . rest)
  (if (exists compute? data)
    (list 'render-text "<plot: waiting for data>")
    (list 'render-plot-line data (if (null? rest) "" (car rest)))))
(define (plot-scatter xs ys . rest)
  (if (or (exists compute? xs) (exists compute? ys))
    (list 'render-text "<plot: waiting for data>")
    (list 'render-plot-scatter xs ys (if (null? rest) "" (car rest)))))
(define (plot-bar labels values . rest)
  (if (exists compute? values)
    (list 'render-text "<plot: waiting for data>")
    (list 'render-plot-bar labels values (if (null? rest) "" (car rest)))))
(define (render . blocks) (list 'render-group blocks))

;; Port declarations are no-ops at runtime (parsed statically by Rust)
(define (input name type) "")
(define (output name type) "")

;; Store: store-get is injected as defines before eval.
;; store-set!/store-append!/store-delete! return tagged lists for Rust to process.
(define (store-set! key value) (list 'store-set key (->str value)))
(define (store-append! key value) (list 'store-append key (->str value)))
(define (store-delete! key) (list 'store-delete key))

;; Interactive widgets (return tagged render blocks)
(define (button label action-expr) (list 'render-button label action-expr))
(define (checkbox label key) (list 'render-checkbox label key))
(define (text-input key . rest) (list 'render-text-input key (if (null? rest) "" (car rest))))
(define (slider key lo hi) (list 'render-slider key lo hi))
"#;

pub struct SchemeEngine {
    env: TopLevelEnvironment,
}

impl SchemeEngine {
    pub fn new() -> Result<Self> {
        let runtime = Runtime::new();
        let env = TopLevelEnvironment::new_repl(&runtime);
        env.eval(true, "(import (rnrs))")
            .map_err(|e| anyhow::anyhow!("Failed to import rnrs: {}", e))?;
        env.eval(false, RENDER_PRELUDE)
            .map_err(|e| anyhow::anyhow!("Failed to load render prelude: {}", e))?;
        log::info!("SchemeEngine ready");
        Ok(Self { env })
    }

    fn make_env(&self) -> Result<TopLevelEnvironment> {
        // Reuse cached env — (rnrs) and prelude already loaded
        // Defines from previous evals persist but get overwritten
        Ok(self.env.clone())
    }

    pub fn eval(&self, code: &str) -> Result<Vec<Value>> {
        let env = self.make_env()?;
        let results = env
            .eval(false, code)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(results)
    }

    pub fn eval_with_bindings(
        &self,
        bindings: &[(String, f64)],
        code: &str,
    ) -> Result<Vec<Value>> {
        let env = self.make_env()?;

        if !bindings.is_empty() {
            let defines: String = bindings
                .iter()
                .map(|(name, val)| format!("(define {} {})", name, val))
                .collect::<Vec<_>>()
                .join(" ");
            env.eval(false, &defines)
                .map_err(|e| anyhow::anyhow!("Binding setup failed: {}", e))?;
        }

        let results = env
            .eval(false, code)
            .map_err(|e| anyhow::anyhow!("Eval failed: {}", e))?;
        Ok(results)
    }

    /// Execute a script node: bind inputs + store, eval code, extract outputs by name
    pub fn execute_script(
        &self,
        input_bindings: &[(String, f64)],
        output_names: &[String],
        store: Option<&crate::store::Store>,
        code: &str,
    ) -> Result<ScriptResult> {
        let env = self.make_env()?;

        // Inject store values as (define (store-get key) ...) via a lookup alist
        if let Some(store) = store {
            let mut alist_items = Vec::new();
            for key in store.keys() {
                if let Some(val) = store.get(&key) {
                    alist_items.push(format!(
                        "(list \"{}\" {})",
                        key,
                        crate::store::Store::value_to_scheme(&val)
                    ));
                }
            }
            let store_define = format!(
                "(define __store (list {})) (define (store-get key) (let ((pair (find (lambda (p) (string=? (car p) key)) __store))) (if pair (car (cdr pair)) \"\")))",
                alist_items.join(" ")
            );
            env.eval(false, &store_define)
                .map_err(|e| anyhow::anyhow!("Store injection failed: {}", e))?;
        }

        if !input_bindings.is_empty() {
            let defines: String = input_bindings
                .iter()
                .map(|(name, val)| format!("(define {} {})", name, val))
                .collect::<Vec<_>>()
                .join(" ");
            env.eval(false, &defines)
                .map_err(|e| anyhow::anyhow!("Binding setup failed: {}", e))?;
        }

        let stripped = preprocessor::preprocess(code);

        let results = env
            .eval(false, &stripped)
            .map_err(|e| anyhow::anyhow!("Eval failed: {}", e))?;

        let mut render_blocks = Vec::new();
        let mut store_mutations = Vec::new();
        for val in &results {
            let display = format!("{}", val);
            if display.starts_with("(store-set ") || display.starts_with("(store-append ") || display.starts_with("(store-delete ") {
                store_mutations.push(display);
            } else if let Some(blocks) = self.try_parse_render(val) {
                render_blocks.extend(blocks);
            }
        }

        // Collect output values by name from environment
        let mut output_values = HashMap::new();
        for name in output_names {
            if let Ok(vals) = env.eval(false, name) {
                if let Some(val) = vals.first() {
                    output_values.insert(name.clone(), value_to_f64_or_string(val));
                }
            }
        }

        Ok(ScriptResult {
            output_values,
            render_blocks,
            store_mutations,
        })
    }

    /// Preview: bind all inputs as <compute> placeholder, eval for structure only
    pub fn preview_script(
        &self,
        input_names: &[String],
        store: Option<&crate::store::Store>,
        code: &str,
    ) -> Result<ScriptResult> {
        let env = self.make_env()?;

        // Inject store values if available, otherwise store-get returns <compute>
        if let Some(store) = store {
            let mut alist_items = Vec::new();
            for key in store.keys() {
                if let Some(val) = store.get(&key) {
                    alist_items.push(format!(
                        "(list \"{}\" {})",
                        key,
                        crate::store::Store::value_to_scheme(&val)
                    ));
                }
            }
            let store_define = format!(
                "(define __store (list {})) (define (store-get key) (let ((pair (find (lambda (p) (string=? (car p) key)) __store))) (if pair (car (cdr pair)) \"\")))",
                alist_items.join(" ")
            );
            env.eval(false, &store_define)
                .map_err(|e| anyhow::anyhow!("Store injection failed: {}", e))?;
        }

        if !input_names.is_empty() {
            let defines: String = input_names
                .iter()
                .map(|name| format!("(define {} <compute>)", name))
                .collect::<Vec<_>>()
                .join(" ");
            env.eval(false, &defines)
                .map_err(|e| anyhow::anyhow!("Preview binding failed: {}", e))?;
        }

        env.eval(false, r#"
            (define + safe+) (define - safe-) (define * safe*) (define / safe/)
            (define min safe-min) (define max safe-max) (define abs safe-abs) (define sqrt safe-sqrt)
            (define (number->string x) (if (compute? x) "<compute>" (->str x)))
            (define (safe-sin x) (if (compute? x) <compute> (sin x)))
            (define (safe-cos x) (if (compute? x) <compute> (cos x)))
            (define sin safe-sin) (define cos safe-cos)
        "#)
            .map_err(|e| anyhow::anyhow!("Safe override failed: {}", e))?;

        let stripped = preprocessor::preprocess(code);

        let results = env
            .eval(false, &stripped)
            .map_err(|e| anyhow::anyhow!("Preview eval failed: {}", e))?;

        let mut render_blocks = Vec::new();
        for val in &results {
            if let Some(blocks) = self.try_parse_render(val) {
                render_blocks.extend(blocks);
            }
        }

        Ok(ScriptResult {
            output_values: HashMap::new(),
            render_blocks,
            store_mutations: Vec::new(),
        })
    }

    fn try_parse_render(&self, val: &Value) -> Option<Vec<RenderBlock>> {
        let display = format!("{}", val);

        // Check if it's a tagged render list
        if !display.starts_with('(') {
            return None;
        }

        parse_render_from_display(&display)
    }

    pub fn value_to_f64(val: &Value) -> Option<f64> {
        val.cast_to_scheme_type::<f64>()
    }

    pub fn value_to_string(val: &Value) -> String {
        format!("{}", val)
    }
}

#[derive(Debug, Clone)]
pub struct ScriptResult {
    pub output_values: HashMap<String, ScriptValue>,
    pub render_blocks: Vec<RenderBlock>,
    pub store_mutations: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ScriptValue {
    Number(f64),
    Str(String),
}

/// Port declaration parsed from Scheme code
#[derive(Debug, Clone)]
pub struct PortDecl {
    pub name: String,
    pub port_type: String, // "f64", "string", etc.
}

/// Parse (input name type) and (output name type) declarations from code
pub fn parse_port_declarations(code: &str) -> (Vec<PortDecl>, Vec<PortDecl>) {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();

    for line in code.lines() {
        let line = line.trim();
        if line.starts_with("(input ") {
            if let Some(decl) = parse_single_decl(line) {
                inputs.push(decl);
            }
        } else if line.starts_with("(output ") {
            if let Some(decl) = parse_single_decl(line) {
                outputs.push(decl);
            }
        }
    }

    (inputs, outputs)
}

fn parse_single_decl(s: &str) -> Option<PortDecl> {
    // "(input x f64)" or "(output sum f64)"
    let s = s.trim();
    let inner = s.strip_prefix('(')?.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split_whitespace().collect();
    if parts.len() >= 3 {
        Some(PortDecl {
            name: parts[1].to_string(),
            port_type: parts[2].to_string(),
        })
    } else {
        None
    }
}

/// Strip (input ...) and (output ...) lines so they don't eval as code
fn strip_declarations(code: &str) -> String {
    code.lines()
        .filter(|line| {
            let t = line.trim();
            !t.starts_with("(input ") && !t.starts_with("(output ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn value_to_f64_or_string(val: &Value) -> ScriptValue {
    if let Some(f) = val.cast_to_scheme_type::<f64>() {
        ScriptValue::Number(f)
    } else {
        ScriptValue::Str(format!("{}", val))
    }
}

// Parse render blocks from Scheme display output like "(render-bold \"hello\")"
fn parse_render_from_display(s: &str) -> Option<Vec<RenderBlock>> {
    let s = s.trim();
    if !s.starts_with('(') || !s.ends_with(')') {
        return None;
    }
    let inner = &s[1..s.len() - 1];

    // Get tag
    let (tag, rest) = split_first_token(inner)?;

    match tag {
        "render-text" => {
            let text = extract_string(rest);
            Some(vec![RenderBlock::Text(text)])
        }
        "render-bold" => {
            let text = extract_string(rest);
            Some(vec![RenderBlock::Bold(text)])
        }
        "render-italic" => {
            let text = extract_string(rest);
            Some(vec![RenderBlock::Italic(text)])
        }
        "render-code" => {
            let text = extract_string(rest);
            Some(vec![RenderBlock::Code(text)])
        }
        "render-link" => {
            let parts = extract_two_strings(rest);
            Some(vec![RenderBlock::Link {
                url: parts.0,
                label: parts.1,
            }])
        }
        "render-hr" => Some(vec![RenderBlock::Hr]),
        "render-table" => {
            let (headers, rows) = parse_table(rest);
            Some(vec![RenderBlock::Table { headers, rows }])
        }
        "render-plot-line" => {
            let (data, title) = parse_plot_line(rest);
            Some(vec![RenderBlock::Plot(PlotData::Line {
                y: data,
                title: if title.is_empty() { None } else { Some(title) },
            })])
        }
        "render-group" => {
            let blocks = parse_group(rest);
            Some(blocks)
        }
        "render-button" => {
            let parts = extract_two_strings(rest);
            // For now, button action is store-set parsed from the second string
            Some(vec![RenderBlock::Button {
                label: parts.0,
                action: crate::render::StoreAction::Set {
                    key: "last-button".to_string(),
                    value: parts.1,
                },
            }])
        }
        "render-checkbox" => {
            let parts = extract_two_strings(rest);
            Some(vec![RenderBlock::Checkbox {
                label: parts.0,
                key: parts.1,
            }])
        }
        "render-text-input" => {
            let parts = extract_two_strings(rest);
            Some(vec![RenderBlock::TextInput {
                key: parts.0,
                placeholder: parts.1,
            }])
        }
        "render-slider" => {
            // (render-slider "key" min max)
            let (key_str, rest2) = find_balanced_parens(rest);
            let key = extract_string(&key_str);
            let nums: Vec<f64> = rest2
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            let min = nums.first().copied().unwrap_or(0.0);
            let max = nums.get(1).copied().unwrap_or(100.0);
            Some(vec![RenderBlock::Slider { key, min, max }])
        }
        _ => None,
    }
}

fn split_first_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(idx) = s.find(|c: char| c.is_whitespace()) {
        Some((s[..idx].trim(), s[idx..].trim()))
    } else {
        Some((s, ""))
    }
}

fn extract_string(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].replace("\\\"", "\"").replace("\\n", "\n")
    } else {
        s.to_string()
    }
}

fn extract_two_strings(s: &str) -> (String, String) {
    let s = s.trim();
    // Find two quoted strings
    let mut strings = Vec::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() && strings.len() < 2 {
        if bytes[i] == b'"' {
            let start = i + 1;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            if i < bytes.len() {
                strings.push(s[start..i].to_string());
            }
        }
        i += 1;
    }
    let first = strings.first().cloned().unwrap_or_default();
    let second = strings.get(1).cloned().unwrap_or_default();
    (first, second)
}

fn parse_list_of_strings(s: &str) -> Vec<String> {
    // Parse scheme list like ("a" "b" "c") or (1 2 3)
    let s = s.trim();
    if s.starts_with('(') && s.ends_with(')') {
        let inner = &s[1..s.len() - 1];
        inner
            .split_whitespace()
            .map(|t| {
                let t = t.trim().trim_matches('"');
                t.to_string()
            })
            .filter(|t| !t.is_empty())
            .collect()
    } else {
        vec![s.to_string()]
    }
}

fn parse_list_of_f64(s: &str) -> Vec<f64> {
    let s = s.trim();
    if s.starts_with('(') && s.ends_with(')') {
        let inner = &s[1..s.len() - 1];
        inner
            .split_whitespace()
            .filter_map(|t| t.trim().parse::<f64>().ok())
            .collect()
    } else {
        Vec::new()
    }
}

fn parse_table(s: &str) -> (Vec<String>, Vec<Vec<String>>) {
    // Expect: ("h1" "h2") (("r1c1" "r1c2") ("r2c1" "r2c2"))
    // For now, simplified parsing
    let s = s.trim();
    let (headers_str, rest) = find_balanced_parens(s);
    let headers = parse_list_of_strings(&headers_str);

    let (rows_str, _) = find_balanced_parens(rest.trim());
    let mut rows = Vec::new();
    let rows_inner = if rows_str.starts_with('(') && rows_str.ends_with(')') {
        &rows_str[1..rows_str.len() - 1]
    } else {
        &rows_str
    };

    let mut remaining = rows_inner.trim();
    while !remaining.is_empty() {
        let (row_str, rest) = find_balanced_parens(remaining);
        if row_str.is_empty() {
            break;
        }
        rows.push(parse_list_of_strings(&row_str));
        remaining = rest.trim();
    }

    (headers, rows)
}

fn parse_plot_line(s: &str) -> (Vec<f64>, String) {
    let s = s.trim();
    let (data_str, rest) = find_balanced_parens(s);
    let data = parse_list_of_f64(&data_str);
    let title = extract_string(rest.trim());
    (data, title)
}

fn parse_group(s: &str) -> Vec<RenderBlock> {
    let s = s.trim();
    let (list_str, _) = find_balanced_parens(s);
    let inner = if list_str.starts_with('(') && list_str.ends_with(')') {
        &list_str[1..list_str.len() - 1]
    } else {
        &list_str
    };

    let mut blocks = Vec::new();
    let mut remaining = inner.trim();
    while !remaining.is_empty() {
        let (item_str, rest) = find_balanced_parens(remaining);
        if item_str.is_empty() {
            break;
        }
        if let Some(parsed) = parse_render_from_display(&item_str) {
            blocks.extend(parsed);
        }
        remaining = rest.trim();
    }
    blocks
}

fn find_balanced_parens(s: &str) -> (String, &str) {
    let s = s.trim();
    if !s.starts_with('(') {
        // Not a paren expression — take until whitespace or end
        if let Some(idx) = s.find(|c: char| c.is_whitespace()) {
            return (s[..idx].to_string(), &s[idx..]);
        }
        return (s.to_string(), "");
    }

    let mut depth = 0;
    let mut in_string = false;
    for (i, c) in s.char_indices() {
        if in_string {
            if c == '\\' {
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return (s[..=i].to_string(), &s[i + 1..]);
                }
            }
            _ => {}
        }
    }
    (s.to_string(), "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_arithmetic() {
        let engine = SchemeEngine::new().unwrap();
        let results = engine.eval("(+ 2 3)").unwrap();
        assert_eq!(results.len(), 1);
        let val = SchemeEngine::value_to_f64(&results[0]);
        println!("(+ 2 3) = {:?}", val);
        assert!(val.is_some());
    }

    #[test]
    fn test_with_bindings() {
        let engine = SchemeEngine::new().unwrap();
        let bindings = vec![("x".to_string(), 7.0), ("y".to_string(), 3.0)];
        let results = engine.eval_with_bindings(&bindings, "(* x y)").unwrap();
        assert_eq!(results.len(), 1);
        let val = SchemeEngine::value_to_f64(&results[0]).unwrap();
        println!("(* 7.0 3.0) = {}", val);
        assert!((val - 21.0).abs() < 1e-10);
    }

    #[test]
    fn test_string_result() {
        let engine = SchemeEngine::new().unwrap();
        let results = engine.eval("\"hello\"").unwrap();
        assert_eq!(results.len(), 1);
        let s = SchemeEngine::value_to_string(&results[0]);
        println!("string = {}", s);
        assert!(s.contains("hello"));
    }

    #[test]
    fn test_render_bold() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine.execute_script(&[], &[], None, "(bold \"hello world\")").unwrap();
        println!("render blocks: {:?}", result.render_blocks);
        assert!(!result.render_blocks.is_empty());
        match &result.render_blocks[0] {
            RenderBlock::Bold(t) => assert!(t.contains("hello")),
            other => panic!("Expected Bold, got {:?}", other),
        }
    }

    #[test]
    fn test_render_group() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine
            .execute_script(
                &[("x".to_string(), 42.0)],
                &[],
                None,
                r#"(render (bold "Result") (text "Value: " (number->string x)))"#,
            )
            .unwrap();
        println!("render blocks: {:?}", result.render_blocks);
        assert!(result.render_blocks.len() >= 2);
    }

    #[test]
    fn test_preview_with_compute() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine
            .preview_script(
                &["x".to_string(), "y".to_string()],
                None,
                r#"(render (bold "Analysis") (text "x + y = " (+ x y)) (hr) (plot-line (list 1 2 x 4) "test"))"#,
            )
            .unwrap();
        println!("preview blocks: {:?}", result.render_blocks);
        assert!(result.render_blocks.len() >= 3);
        // bold "Analysis" should render normally
        match &result.render_blocks[0] {
            RenderBlock::Bold(t) => assert_eq!(t, "Analysis"),
            other => panic!("Expected Bold, got {:?}", other),
        }
        // text should contain <compute>
        match &result.render_blocks[1] {
            RenderBlock::Text(t) => {
                println!("text: {}", t);
                assert!(t.contains("<compute>"));
            }
            other => panic!("Expected Text, got {:?}", other),
        }
    }
}
