use crate::preprocessor;
use crate::render::RenderBlock;
use crate::scheme_convert::try_parse_render_from_value;
pub use crate::scheme_convert::{parse_port_declarations, scheme_value_to_json};
use anyhow::Result;
use scheme_rs::env::TopLevelEnvironment;
use scheme_rs::runtime::Runtime;
use scheme_rs::value::Value;
use std::collections::HashMap;

const CANVAS_RENDER_LIB: &str = r#"
(library (canvas render)
  (export <compute> compute? ->str
          text bold italic code link hr table render
          plot-line plot-scatter plot-bar
          numbered-list bullet-list
          button checkbox text-input slider editable-list
          json-null)
  (import (rnrs))

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
      ((null? x) "")
      ((pair? x)
       (string-append "(" (let loop ((lst x))
         (cond
           ((null? lst) ")")
           ((pair? lst)
            (string-append (->str (car lst))
                           (if (null? (cdr lst)) ")" (string-append ", " (loop (cdr lst))))))
           (else (string-append ". " (->str lst) ")"))))))
      (else "?")))

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

  ;; List rendering helpers
  (define (numbered-list items)
    (if (or (null? items) (string? items))
      (list 'render-text "(empty)")
      (list 'render-group
        (let loop ((lst items) (i 1))
          (if (null? lst) '()
            (cons (list 'render-text (string-append (number->string i) ". " (->str (car lst))))
                  (loop (cdr lst) (+ i 1))))))))
  (define (bullet-list items)
    (if (or (null? items) (string? items))
      (list 'render-text "(empty)")
      (list 'render-group
        (let loop ((lst items))
          (if (null? lst) '()
            (cons (list 'render-text (string-append "  - " (->str (car lst))))
                  (loop (cdr lst))))))))

  ;; JSON null sentinel
  (define json-null 'json-null)

  ;; Interactive widgets — each gets its own tagged list
  (define (button label action-type . args)
    (list 'render-button label (symbol->string action-type) args))
  (define (checkbox label key)
    (list 'render-checkbox label key))
  (define (text-input key . rest)
    (list 'render-text-input key (if (null? rest) "" (car rest))))
  (define (slider key lo hi)
    (list 'render-slider key lo hi))
  (define (editable-list key)
    (list 'render-editable-list key))
)
"#;

const CANVAS_PREVIEW_LIB: &str = r#"
(library (canvas preview)
  (export safe+ safe- safe* safe/
          safe-min safe-max safe-abs safe-sqrt)
  (import (rnrs) (canvas render))

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
)
"#;

pub struct SchemeEngine {
    pub runtime: Runtime,
    env: TopLevelEnvironment,
}

impl SchemeEngine {
    pub fn new() -> Result<Self> {
        // Set up user library path (don't override if already set)
        if std::env::var("SCHEME_RS_LOAD_PATH").is_err() {
            let lib_dir = dirs::home_dir()
                .unwrap_or_default()
                .join(".canvas")
                .join("lib");
            std::fs::create_dir_all(&lib_dir).ok();
            std::env::set_var("SCHEME_RS_LOAD_PATH", &lib_dir);
        }

        let runtime = Runtime::new();

        runtime.def_lib(CANVAS_RENDER_LIB)
            .map_err(|e| anyhow::anyhow!("Failed to define (canvas render): {}", e))?;
        runtime.def_lib(CANVAS_PREVIEW_LIB)
            .map_err(|e| anyhow::anyhow!("Failed to define (canvas preview): {}", e))?;

        let env = TopLevelEnvironment::new_repl(&runtime);
        env.eval(true, "(import (rnrs))")
            .map_err(|e| anyhow::anyhow!("Failed to import rnrs: {}", e))?;
        env.eval(true, "(import (canvas db))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas db: {}", e))?;
        env.eval(true, "(import (canvas render))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas render: {}", e))?;

        // Port declarations are no-ops at runtime (parsed statically by Rust)
        env.eval(false, "(define (input name type) \"\") (define (output name type) \"\")")
            .map_err(|e| anyhow::anyhow!("Failed to define input/output stubs: {}", e))?;

        log::info!("SchemeEngine ready");
        Ok(Self { runtime, env })
    }

    pub fn make_env(&self) -> TopLevelEnvironment {
        self.env.clone()
    }

    /// Create a persistent REPL environment with all canvas libraries imported.
    pub fn make_repl_env(&self) -> Result<TopLevelEnvironment> {
        let env = TopLevelEnvironment::new_repl(&self.runtime);
        env.eval(true, "(import (rnrs) (canvas db) (canvas render) (canvas graph))")
            .map_err(|e| anyhow::anyhow!("Failed to setup REPL env: {}", e))?;
        env.eval(false, "(define (input name type) \"\") (define (output name type) \"\")")
            .map_err(|e| anyhow::anyhow!("Failed to define stubs in REPL env: {}", e))?;
        Ok(env)
    }

    /// Eval code in a given REPL env (persistent across calls).
    pub fn eval_repl(
        &self,
        env: &TopLevelEnvironment,
        db: Option<&crate::db::Db>,
        graph: Option<(&mut crate::types::Graph, &crate::registry::NodeRegistry)>,
        code: &str,
    ) -> Result<Vec<Value>> {
        let eval_fn = || {
            env.eval(true, code)
                .map_err(|e| anyhow::anyhow!("REPL eval failed: {}", e))
        };

        // Set up both db and graph contexts
        match (db, graph) {
            (Some(db), Some((graph, registry))) => {
                crate::bridge::with_db_context(db, || {
                    crate::bridge::with_graph_context(graph, registry, eval_fn)
                })
            }
            (Some(db), None) => {
                crate::bridge::with_db_context(db, eval_fn)
            }
            (None, Some((graph, registry))) => {
                crate::bridge::with_graph_context(graph, registry, eval_fn)
            }
            (None, None) => eval_fn(),
        }
    }

    /// Register a node's outputs as an R6RS library `(node <id>)`.
    /// After this, script nodes can do `(import (node 3))` to access upstream outputs.
    pub fn register_node_library(
        &self,
        node_id: crate::types::NodeId,
        outputs: &HashMap<String, crate::types::Value>,
    ) {
        if outputs.is_empty() {
            return;
        }

        let exports: Vec<String> = outputs.keys().cloned().collect();
        let defines: Vec<String> = outputs
            .iter()
            .map(|(name, val)| format!("(define {} {})", name, val.to_scheme_literal()))
            .collect();

        let lib_str = format!(
            "(library (node n{}) (export {}) (import (rnrs)) {})",
            node_id,
            exports.join(" "),
            defines.join(" ")
        );

        if let Err(e) = self.runtime.def_lib(&lib_str) {
            log::warn!("Failed to register node {} library: {}", node_id, e);
        }
    }

    pub fn eval(&self, code: &str) -> Result<Vec<Value>> {
        let env = self.make_env();
        let results = env
            .eval(false, code)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(results)
    }

    /// Execute a script node: bind inputs + db, eval code, extract outputs by name
    pub fn execute_script(
        &self,
        input_bindings: &[(String, crate::types::Value)],
        output_names: &[String],
        db: Option<&crate::db::Db>,
        code: &str,
    ) -> Result<ScriptResult> {
        let env = self.make_env();

        let stripped = preprocessor::preprocess(code);

        if !input_bindings.is_empty() {
            let defines: String = input_bindings
                .iter()
                .map(|(name, val)| format!("(define {} {})", name, val.to_scheme_literal()))
                .collect::<Vec<_>>()
                .join(" ");
            env.eval(false, &defines)
                .map_err(|e| anyhow::anyhow!("Binding setup failed: {}", e))?;
        }

        let eval_fn = || {
            env.eval(true, &stripped)
                .map_err(|e| anyhow::anyhow!("Eval failed: {}", e))
        };

        let results = if let Some(db) = db {
            crate::bridge::with_db_context(db, eval_fn)?
        } else {
            eval_fn()?
        };

        let mut render_blocks = Vec::new();
        for val in &results {
            if let Some(blocks) = try_parse_render_from_value(val) {
                render_blocks.extend(blocks);
            }
        }

        // Collect output values by name from environment
        let mut output_values = HashMap::new();
        for name in output_names {
            if let Ok(vals) = env.eval(false, name) {
                if let Some(val) = vals.first() {
                    let typed_val = if let Some(f) = val.cast_to_scheme_type::<f64>() {
                        crate::types::Value::F64(f)
                    } else {
                        crate::types::Value::Str(format!("{}", val))
                    };
                    output_values.insert(name.clone(), typed_val);
                }
            }
        }

        Ok(ScriptResult {
            output_values,
            render_blocks,
        })
    }

    /// Preview: bind all inputs as <compute> placeholder, eval for structure only
    pub fn preview_script(
        &self,
        input_names: &[String],
        db: Option<&crate::db::Db>,
        code: &str,
    ) -> Result<ScriptResult> {
        let env = self.make_env();

        let stripped = preprocessor::preprocess(code);

        if !input_names.is_empty() {
            let defines: String = input_names
                .iter()
                .map(|name| format!("(define {} <compute>)", name))
                .collect::<Vec<_>>()
                .join(" ");
            env.eval(false, &defines)
                .map_err(|e| anyhow::anyhow!("Preview binding failed: {}", e))?;
        }

        env.eval(true, "(import (canvas preview))")
            .map_err(|e| anyhow::anyhow!("Preview import failed: {}", e))?;
        env.eval(false, r#"
            (define + safe+) (define - safe-) (define * safe*) (define / safe/)
            (define min safe-min) (define max safe-max) (define abs safe-abs) (define sqrt safe-sqrt)
            (define (number->string x) (if (compute? x) "<compute>" (->str x)))
            (define (safe-sin x) (if (compute? x) <compute> (sin x)))
            (define (safe-cos x) (if (compute? x) <compute> (cos x)))
            (define sin safe-sin) (define cos safe-cos)
        "#)
            .map_err(|e| anyhow::anyhow!("Safe override failed: {}", e))?;

        let eval_fn = || {
            env.eval(false, &stripped)
                .map_err(|e| anyhow::anyhow!("Preview eval failed: {}", e))
        };

        let results = if let Some(db) = db {
            crate::bridge::with_db_context(db, eval_fn)?
        } else {
            eval_fn()?
        };

        let mut render_blocks = Vec::new();
        for val in &results {
            if let Some(blocks) = try_parse_render_from_value(val) {
                render_blocks.extend(blocks);
            }
        }

        Ok(ScriptResult {
            output_values: HashMap::new(),
            render_blocks,
        })
    }

}

#[derive(Debug, Clone)]
pub struct ScriptResult {
    pub output_values: HashMap<String, crate::types::Value>,
    pub render_blocks: Vec<RenderBlock>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_arithmetic() {
        let engine = SchemeEngine::new().unwrap();
        let results = engine.eval("(+ 2 3)").unwrap();
        assert_eq!(results.len(), 1);
        let val = results[0].cast_to_scheme_type::<f64>();
        println!("(+ 2 3) = {:?}", val);
        assert!(val.is_some());
    }

    #[test]
    fn test_with_bindings() {
        let engine = SchemeEngine::new().unwrap();
        let bindings = vec![
            ("x".to_string(), crate::types::Value::F64(7.0)),
            ("y".to_string(), crate::types::Value::F64(3.0)),
        ];
        let result = engine.execute_script(&bindings, &["result".to_string()], None, "(define result (* x y))").unwrap();
        match result.output_values.get("result") {
            Some(crate::types::Value::F64(v)) => assert!((*v - 21.0).abs() < 1e-10),
            other => panic!("Expected F64(21.0), got {:?}", other),
        }
    }

    #[test]
    fn test_string_result() {
        let engine = SchemeEngine::new().unwrap();
        let results = engine.eval("\"hello\"").unwrap();
        assert_eq!(results.len(), 1);
        let s = format!("{}", results[0]);
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
                &[("x".to_string(), crate::types::Value::F64(42.0))],
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

    #[test]
    fn test_button_parsing() {
        let engine = SchemeEngine::new().unwrap();

        // Test standalone button
        let results = engine.eval(r#"(button "Add Task" 'append "tasks" "new-task")"#).unwrap();
        for r in &results {
            let display = format!("{}", r);
            println!("button standalone: {:?}", display);
            assert!(display.contains("render-button"));
        }

        // Test button inside render group — first see raw output
        let raw = engine.eval(r#"(render (bold "Test") (button "Click Me" 'set "key1" "val1"))"#).unwrap();
        for r in &raw {
            println!("render-group raw: {:?}", format!("{}", r));
        }

        let result = engine
            .execute_script(
                &[],
                &[],
                None,
                r#"(render (bold "Test") (button "Click Me" 'set "key1" "val1"))"#,
            )
            .unwrap();
        println!("button render blocks: {:?}", result.render_blocks);

        let has_button = result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Button { label, .. } if label == "Click Me"));
        assert!(has_button, "Expected a Button with label 'Click Me', got: {:?}", result.render_blocks);
    }

    #[test]
    fn test_bridge_store_roundtrip() {
        use serde_json::json;

        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();

        // store-set! then store-get in same script works via bridge
        let result = engine
            .execute_script(
                &[],
                &[],
                Some(&db),
                r#"(store-set! "k" "v") (store-get "k")"#,
            )
            .unwrap();
        // Verify via direct DB access
        assert_eq!(db.kv_get("k"), Some(json!("v")));
    }

    #[test]
    fn test_bridge_db_query_live() {
        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();

        let result = engine
            .execute_script(
                &[],
                &[],
                Some(&db),
                r#"(db-run "CREATE items SET name = 'apple', qty = 3") (db-query "SELECT * FROM items")"#,
            )
            .unwrap();
        // Verify data exists
        let rows = db.query("SELECT * FROM items").unwrap();
        assert!(!rows.is_empty());
    }

    #[test]
    fn test_bridge_store_keys() {
        use serde_json::json;

        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();

        let _result = engine
            .execute_script(
                &[],
                &[],
                Some(&db),
                r#"(store-set! "a" 1) (store-set! "b" 2) (store-keys)"#,
            )
            .unwrap();
        let mut keys = db.kv_keys();
        keys.sort();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[test]
    fn test_checkbox_and_input() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine
            .execute_script(
                &[],
                &[],
                None,
                r#"(render (checkbox "Enabled" "is-on") (text-input "name" "Enter..."))"#,
            )
            .unwrap();
        println!("widget blocks: {:?}", result.render_blocks);
        assert!(result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Checkbox { label, .. } if label == "Enabled")));
        assert!(result.render_blocks.iter().any(|b| matches!(b, RenderBlock::TextInput { key, .. } if key == "name")));
    }

    #[test]
    fn test_import_canvas_render() {
        let engine = SchemeEngine::new().unwrap();
        // Import should work in fresh env
        let env = engine.make_env();
        let result = env.eval(true, "(import (canvas render))");
        assert!(result.is_ok(), "Failed to import (canvas render): {:?}", result.err());
    }

    #[test]
    fn test_load_user_library() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("canvas-test-libs");
        std::fs::create_dir_all(&dir).unwrap();
        let lib_file = dir.join("mylib.sls");
        let mut f = std::fs::File::create(&lib_file).unwrap();
        writeln!(f, r#"(library (mylib) (export greet) (import (rnrs)) (define (greet name) (string-append "Hello, " name "!")))"#).unwrap();

        std::env::set_var("SCHEME_RS_LOAD_PATH", &dir);
        let engine = SchemeEngine::new().unwrap();
        let result = engine.execute_script(
            &[],
            &["msg".to_string()],
            None,
            r#"(import (mylib)) (define msg (greet "World"))"#,
        );
        // Clean up
        std::fs::remove_file(&lib_file).ok();
        std::fs::remove_dir(&dir).ok();

        let result = result.unwrap();
        match result.output_values.get("msg") {
            Some(crate::types::Value::Str(s)) => assert!(s.contains("Hello"), "Got: {}", s),
            other => panic!("Expected string output, got: {:?}", other),
        }
    }

    #[test]
    fn test_missing_library_error() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine.execute_script(
            &[],
            &[],
            None,
            "(import (nonexistent-lib-xyz))",
        );
        assert!(result.is_err(), "Expected error for missing library");
    }

    #[test]
    fn test_scribble_to_render_pipeline() {
        let engine = SchemeEngine::new().unwrap();
        let code = "(input x f64)\n(output result f64)\n(define result (* x 2))\n\n# Title\n\nResult is @result.";
        let result = engine.execute_script(
            &[("x".to_string(), crate::types::Value::F64(5.0))],
            &["result".to_string()],
            None,
            code,
        ).unwrap();
        assert!(matches!(result.output_values.get("result"),
            Some(crate::types::Value::F64(v)) if (*v - 10.0).abs() < f64::EPSILON));
        assert!(!result.render_blocks.is_empty());
        assert!(result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Bold(_))));
        assert!(result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Text(t) if t.contains("10"))));
    }
}
