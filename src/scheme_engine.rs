use crate::render::RenderBlock;
use crate::scheme_convert::try_parse_render_from_value;
pub use crate::scheme_convert::scheme_value_to_json;
use anyhow::Result;
use scheme_rs::env::TopLevelEnvironment;
use scheme_rs::runtime::Runtime;
use scheme_rs::value::Value;
use std::collections::HashMap;

/// Convert a Scheme runtime Value into a crate::types::Value.
pub fn scheme_value_to_types_value(val: &Value) -> crate::types::Value {
    if let Some(f) = val.cast_to_scheme_type::<f64>() {
        crate::types::Value::F64(f)
    } else {
        let s = format!("{}", val);
        match s.as_str() {
            "#t" | "true" => crate::types::Value::Bool(true),
            "#f" | "false" => crate::types::Value::Bool(false),
            _ => crate::types::Value::Str(s),
        }
    }
}

const CANVAS_RENDER_LIB: &str = r#"
(library (canvas render)
  (export <compute> compute? ->str
          text bold italic code link hr table render render-map
          plot-line plot-scatter plot-bar
          numbered-list bullet-list
          button checkbox text-input editable-list slider
          json-null
          canvas draw-line draw-rect draw-circle draw-polyline draw-text
          row group node-blocks node-widgets
          interactive on)
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

  ;; Dynamic list rendering: call fn for each element with (item index)
  (define (render-map lst fn)
    (list 'render-group
      (let loop ((rest lst) (i 0))
        (if (or (null? rest) (not (pair? rest))) '()
          (cons (fn (car rest) i)
                (loop (cdr rest) (+ i 1)))))))

  ;; Generic canvas drawing
  (define (canvas w h . cmds) (list 'render-canvas w h cmds))
  (define (draw-line x1 y1 x2 y2 color width) (list 'line x1 y1 x2 y2 color width))
  (define (draw-rect x y w h fill) (list 'rect x y w h fill))
  (define (draw-circle x y r fill) (list 'circle x y r fill))
  (define (draw-polyline pts color width) (list 'polyline pts color width))
  (define (draw-text x y txt color size) (list 'text x y txt color size))

  ;; Layout & composition
  (define (row . items) (list 'render-row items))
  (define (group . items) (list 'render-frame items))
  (define (node-blocks label) (list 'render-node-blocks label))
  (define (node-widgets label) (list 'render-node-widgets label))

  ;; Event handling
  (define (on event-type message) (list 'event (symbol->string event-type) message))
  (define (interactive . args)
    (let loop ((rest args) (events '()) (children '()))
      (if (null? rest)
        (list 'render-interactive events (reverse children))
        (let ((item (car rest)))
          (if (and (pair? item) (eq? (car item) 'event))
            (loop (cdr rest) (cons (cdr item) events) children)
            (loop (cdr rest) events (cons item children)))))))
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

/// Module header parsed from `(define-module ...)` in node script code.
#[derive(Debug, Clone, Default)]
pub struct ModuleHeader {
    pub canvas: String,
    pub name: String,
    pub exports: Vec<String>,
    /// All imports: (canvas_name, module_name) from (use-module (canvas module))
    pub imports: Vec<(String, String)>,
}

/// Parse `(define-module ...)` header from script code textually.
/// Returns None if no define-module found.
///
/// Format:
/// ```scheme
/// (define-module (my-canvas controls)
///   (export gain freq)
///   (use-module (my-canvas wave))
///   (use-module (other-canvas sensors)))
/// ```
/// Migration: used by migrate_module_header() to extract metadata from legacy code.
pub fn parse_module_header(code: &str) -> Option<ModuleHeader> {
    let dm_start = code.find("(define-module")?;
    // Find the matching closing paren for the define-module form
    let dm_end = find_matching_paren(code, dm_start)?;
    let form = &code[dm_start..=dm_end];

    let mut header = ModuleHeader::default();

    // Parse (canvas-name module-name) after define-module
    let after_dm = "define-module".len() + 1; // skip past "(define-module"
    if let Some(paren_pos) = form[after_dm..].find('(') {
        let inner_start = after_dm + paren_pos + 1;
        if let Some(paren_end) = form[inner_start..].find(')') {
            let inner = form[inner_start..inner_start + paren_end].trim();
            let parts: Vec<&str> = inner.split_whitespace().collect();
            if parts.len() == 2 {
                header.canvas = parts[0].to_string();
                header.name = parts[1].to_string();
            }
        }
    }

    // Parse (export sym1 sym2 ...)
    if let Some(pos) = form.find("(export ") {
        let after = pos + 8;
        if let Some(end) = form[after..].find(')') {
            let syms = &form[after..after + end];
            header.exports = syms.split_whitespace().map(|s| s.to_string()).collect();
        }
    }

    // Parse (use-module (canvas-name module-name)) — may appear multiple times
    let use_pat = "(use-module (";
    let mut start = 0;
    while let Some(pos) = form[start..].find(use_pat) {
        let abs = start + pos + use_pat.len();
        if let Some(end) = form[abs..].find("))") {
            let inner = form[abs..abs + end].trim();
            let parts: Vec<&str> = inner.split_whitespace().collect();
            if parts.len() == 2 {
                header.imports.push((parts[0].to_string(), parts[1].to_string()));
            }
        }
        start = abs;
    }

    Some(header)
}

/// Find the closing paren matching the opening paren at `start`.
fn find_matching_paren(code: &str, start: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut depth = 0i32;
    let mut i = start;
    let mut in_string = false;
    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\\' { i += 2; continue; }
            if bytes[i] == b'"' { in_string = false; }
            i += 1;
            continue;
        }
        match bytes[i] {
            b'"' => { in_string = true; i += 1; }
            b';' => { while i < bytes.len() && bytes[i] != b'\n' { i += 1; } }
            b'(' => { depth += 1; i += 1; }
            b')' => {
                depth -= 1;
                if depth == 0 { return Some(i); }
                i += 1;
            }
            _ => { i += 1; }
        }
    }
    None
}

/// Migration helper: extract imports from code as (canvas_name, module_name) pairs.
/// Prefer reading node.imports directly — this function is for backward compat only.
pub fn extract_imports(code: &str) -> Vec<(String, String)> {
    if let Some(header) = parse_module_header(code) {
        return header.imports;
    }
    Vec::new()
}

/// Migration: extract define-module metadata into Node fields and strip header from code.
/// Idempotent — skips if node already has exports or imports populated.
pub fn migrate_module_header(node: &mut crate::types::Node) {
    if !node.exports.is_empty() || !node.imports.is_empty() {
        return;
    }
    if let Some(header) = parse_module_header(&node.script_code) {
        node.exports = header.exports;
        node.imports = header.imports;
        let (body, _) = strip_module_header(&node.script_code);
        node.script_code = body;
        node.recompute_hash();
    }
}

/// Strip `(define-module ...)` form from code, returning the body.
/// Also returns import statements to prepend.
pub fn strip_module_header(code: &str) -> (String, Vec<String>) {
    if let Some(header) = parse_module_header(code) {
        let dm_start = code.find("(define-module").unwrap();
        let dm_end = find_matching_paren(code, dm_start).unwrap();
        let before = &code[..dm_start];
        let after = &code[dm_end + 1..];
        let body = format!("{}{}", before.trim_end(), after);
        let imports: Vec<String> = header.imports.iter()
            .map(|(canvas, module)| format!("(import ({} {}))", canvas, module))
            .collect();
        (body.trim_start_matches('\n').to_string(), imports)
    } else {
        (code.to_string(), Vec::new())
    }
}

const PORT_WRAPPERS: &str = r#"
    (define (widget name type . params)
      (register-widget name type
        (if (null? params) 0 (car params))
        (if (or (null? params) (null? (cdr params))) 0 (cadr params))))
    (define (slider name lo hi) (register-widget name 'slider lo hi))
    (define (checkbox name) (register-widget name 'checkbox 0 0))
"#;

/// Eval code within bridge port+db context, extract render blocks from results.
fn eval_with_bridge<F>(
    available_inputs: Option<&HashMap<String, crate::types::Value>>,
    db: Option<&crate::db::Db>,
    f: F,
) -> (Result<Vec<Value>>, crate::bridge::PortRegistry)
where
    F: FnOnce() -> Result<Vec<Value>>,
{
    crate::bridge::with_port_context(available_inputs, || {
        let eval_fn = || f();
        if let Some(db) = db {
            crate::bridge::with_db_context(db, eval_fn)
        } else {
            eval_fn()
        }
    })
}

fn extract_render_blocks(results: &[Value]) -> Vec<RenderBlock> {
    let mut blocks = Vec::new();
    for val in results {
        if let Some(b) = try_parse_render_from_value(val) {
            blocks.extend(b);
        }
    }
    blocks
}

fn collect_side_effects() -> (Option<u64>, Vec<crate::bridge::OCapNSendEntry>, Vec<crate::types::NodeId>, bool, Option<String>) {
    (
        crate::bridge::take_tick_interval(),
        crate::bridge::take_ocapn_sends(),
        crate::bridge::take_recompute_requests(),
        crate::bridge::take_has_message_handler(),
        crate::bridge::take_window_title(),
    )
}

pub struct SchemeEngine {
    pub runtime: Runtime,
    env: TopLevelEnvironment,
}

impl SchemeEngine {
    pub fn new() -> Result<Self> {
        // Set up user library path (don't override if already set)
        if std::env::var("SCHEME_RS_LOAD_PATH").is_err() {
            let lib_dir = dirs::data_local_dir()
                .or_else(|| dirs::home_dir())
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
        env.eval(true, "(import (canvas ports))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas ports: {}", e))?;
        env.eval(true, "(import (canvas net))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas net: {}", e))?;
        env.eval(true, "(import (canvas timer))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas timer: {}", e))?;
        env.eval(true, "(import (canvas ocapn))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas ocapn: {}", e))?;
        env.eval(true, "(import (canvas actor))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas actor: {}", e))?;

        // Port declaration wrappers: call bridge functions from (canvas ports)
        env.eval(false, PORT_WRAPPERS)
            .map_err(|e| anyhow::anyhow!("Failed to define port wrappers: {}", e))?;
        env.eval(false, r#"
            (define (net-publish channel) (net-publish-channel channel))
            (define (net-value channel key default) (net-value-get channel key default))
            (define (request-tick ms) (request-tick-ms ms))
            (define (ocapn-export val) (ocapn-export-value val))
            (define (ocapn-send locator method . args) (ocapn-send-msg locator method args))
            (define (ocapn-receive uri default) (ocapn-receive-msg uri default))
            (define (ocapn-export-node id) (ocapn-export-node-id id))
            (define (ocapn-peers) (ocapn-peers-list))
            (define (ocapn-local-id) (ocapn-local-id-get))
            (define (ocapn-call locator method . args) (ocapn-call-msg locator method args))
            (define (ocapn-call-result request-id default) (ocapn-call-result-get request-id default))
            (define (node-id) (actor-node-id))
            (define (node-address) (actor-node-address))
            (define (node-send target method . args) (actor-send-msg target method args))
            (define (receive) (actor-receive-msg))
            (define (mailbox-count) (actor-mailbox-count))
            (define (self-send method . args) (actor-self-send-msg method args))
            (define (open-window title) (actor-open-window title))
            (define (__state-key name)
              (string-append "__s:" (number->string (node-id)) ":" (symbol->string name)))
            (define (state name default)
              (let ((key (__state-key name)))
                (let ((v (store-get key)))
                  (if (equal? v "") default v))))
            (define (set-state! name value)
              (store-set! (__state-key name) value)
              value)
            (define __actor-msg-handler #f)
            (define (on-message handler)
              (actor-register-handler)
              (set! __actor-msg-handler handler)
              (let loop ()
                (let ((msg (receive)))
                  (when msg
                    (handler msg)
                    (loop)))))
            (define (__actor-drain-messages)
              (when __actor-msg-handler
                (let loop ()
                  (let ((msg (receive)))
                    (when msg
                      (__actor-msg-handler msg)
                      (loop))))))
        "#)
            .map_err(|e| anyhow::anyhow!("Failed to define port wrappers: {}", e))?;

        log::info!("SchemeEngine ready");
        Ok(Self { runtime, env })
    }

    pub fn make_env(&self) -> TopLevelEnvironment {
        self.env.clone()
    }

    /// Create a persistent REPL environment with all canvas libraries imported.
    pub fn make_repl_env(&self) -> Result<TopLevelEnvironment> {
        let env = TopLevelEnvironment::new_repl(&self.runtime);
        env.eval(true, "(import (rnrs) (canvas db) (canvas render) (canvas graph) (canvas ports))")
            .map_err(|e| anyhow::anyhow!("Failed to setup REPL env: {}", e))?;
        env.eval(false, PORT_WRAPPERS)
            .map_err(|e| anyhow::anyhow!("Failed to define port wrappers in REPL env: {}", e))?;
        Ok(env)
    }

    /// Register stub libraries for all nodes with exports.
    /// All exports get sentinel values. This ensures (import (canvas module))
    /// never fails, even before first compute.
    pub fn register_stub_libraries(&self, canvas_name: &str, nodes: &std::collections::HashMap<crate::types::NodeId, crate::types::Node>) {
        for (node_id, node) in nodes {
            let module_name = node.label.replace(' ', "-");
            if node.exports.is_empty() || module_name.is_empty() {
                continue;
            }
            let mut stub_outputs = std::collections::HashMap::new();
            for export_name in &node.exports {
                let val = node.output_values.get(export_name).cloned()
                    .unwrap_or(crate::types::Value::F64(0.0));
                stub_outputs.insert(export_name.clone(), val);
            }
            self.register_node_library_named(*node_id, canvas_name, &module_name, &stub_outputs);
        }
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

    /// Register a node's outputs as an R6RS library `(canvas-name module-name)`.
    pub fn register_node_library_named(
        &self,
        _node_id: crate::types::NodeId,
        canvas_name: &str,
        module_name: &str,
        outputs: &HashMap<String, crate::types::Value>,
    ) {
        if outputs.is_empty() || canvas_name.is_empty() || module_name.is_empty() {
            return;
        }

        let exports: Vec<String> = outputs.keys().cloned().collect();
        let defines: Vec<String> = outputs
            .iter()
            .map(|(name, val)| format!("(define {} {})", name, val.to_scheme_literal()))
            .collect();

        let export_str = exports.join(" ");
        let define_str = defines.join(" ");

        let safe_module = module_name.replace(' ', "-");
        let safe_canvas = canvas_name.replace(' ', "-");

        // Self-reference: (import (canvas controls)) gives binding `controls` = "controls"
        let self_ref_export = if exports.contains(&safe_module) {
            String::new()
        } else {
            format!(" {}", safe_module)
        };
        let self_ref_define = if exports.contains(&safe_module) {
            String::new()
        } else {
            format!(" (define {} \"{}\")", safe_module, safe_module)
        };
        // Export <module>-widgets and <module>-blocks as render block values
        let widgets_name = format!("{}-widgets", safe_module);
        let blocks_name = format!("{}-blocks", safe_module);
        let extra_exports = format!(" {} {}", widgets_name, blocks_name);
        let extra_defines = format!(
            " (define {} (list 'render-node-widgets \"{}\")) (define {} (list 'render-node-blocks \"{}\"))",
            widgets_name, safe_module, blocks_name, safe_module
        );
        let lib_str = format!(
            "(library ({} {}) (export {}{}{}) (import (rnrs)) {}{}{})",
            safe_canvas, safe_module, export_str, self_ref_export, extra_exports,
            define_str, self_ref_define, extra_defines
        );
        if let Err(e) = self.runtime.def_lib(&lib_str) {
            log::warn!("Failed to register library ({} {}): {}", canvas_name, module_name, e);
        }
    }

    /// Split code string into individual top-level S-expressions.
    /// Tracks paren depth and string literals to find form boundaries.
    fn split_toplevel_forms(code: &str) -> Vec<String> {
        let mut forms = Vec::new();
        let mut depth: i32 = 0;
        let mut start = None;
        let mut in_string = false;
        let mut escape = false;

        for (i, c) in code.char_indices() {
            if escape {
                escape = false;
                continue;
            }
            if c == '\\' && in_string {
                escape = true;
                continue;
            }
            if c == '"' {
                in_string = !in_string;
                continue;
            }
            if in_string {
                continue;
            }

            if c == '(' {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            } else if c == ')' {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        forms.push(code[s..=i].to_string());
                        start = None;
                    }
                }
            }
        }
        forms
    }

    /// Eval module-stripped code form-by-form.
    /// Imports need `eval(true, ...)`. Everything else uses `eval(false, ...)`
    /// which persists defines in the REPL env and allows runtime variable lookup.
    fn eval_forms(env: &TopLevelEnvironment, code: &str) -> Result<Vec<Value>> {
        let forms = Self::split_toplevel_forms(code);
        let mut results = Vec::new();
        for form in &forms {
            let trimmed = form.trim();
            if trimmed.is_empty() {
                continue;
            }
            let is_import = trimmed.starts_with("(import ");
            // Canvas module imports have two words: (import (canvas-name module-name))
            let is_module_import = is_import && {
                if let Some(inner) = trimmed.strip_prefix("(import (").and_then(|s| s.strip_suffix("))")) {
                    inner.trim().split_whitespace().count() == 2
                } else {
                    false
                }
            };
            match env.eval(is_import, trimmed) {
                Ok(r) => results.extend(r),
                Err(e) if is_module_import => {
                    // Gracefully skip module imports if library not registered yet.
                    // Define fallback bindings for label, label-widgets, label-blocks.
                    log::debug!("Module import skipped: {}", e);
                    // Extract module name (last word before closing parens)
                    let inner = trimmed.strip_prefix("(import (")
                        .and_then(|s| s.strip_suffix("))"));
                    if let Some(inner) = inner {
                        let parts: Vec<&str> = inner.trim().split_whitespace().collect();
                        let label = parts.last().map(|s| s.trim()).unwrap_or("");
                        if !label.is_empty() {
                            let _ = env.eval(false, &format!(
                                "(define {} \"{}\") (define {}-widgets (list 'render-node-widgets \"{}\")) (define {}-blocks (list 'render-node-blocks \"{}\"))",
                                label, label, label, label, label, label
                            ));
                        }
                    }
                }
                Err(e) => return Err(anyhow::anyhow!("Eval failed: {}", e)),
            }
        }
        Ok(results)
    }

    pub fn eval(&self, code: &str) -> Result<Vec<Value>> {
        let env = self.make_env();
        let results = env
            .eval(false, code)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(results)
    }

    /// Execute a script node: bind inputs via bridge, eval code, extract outputs dynamically
    pub fn execute_script(
        &self,
        available_inputs: &HashMap<String, crate::types::Value>,
        db: Option<&crate::db::Db>,
        code: &str,
    ) -> Result<ScriptResult> {
        let (result, _env, _preprocessed) = self.execute_script_cached(None, available_inputs, db, code, &[], &[])?;
        Ok(result)
    }

    /// Execute a script, optionally reusing a cached environment.
    /// Returns (result, env, eval_code) — caller can cache env and eval_code for next eval.
    /// When env is reused, `set!` mutations persist between computes.
    ///
    /// `exports` and `imports` come from Node struct fields (not from code).
    /// `code` is pure Scheme without define-module header.
    pub fn execute_script_cached(
        &self,
        cached_env: Option<TopLevelEnvironment>,
        available_inputs: &HashMap<String, crate::types::Value>,
        db: Option<&crate::db::Db>,
        code: &str,
        exports: &[String],
        imports: &[(String, String)],
    ) -> Result<(ScriptResult, TopLevelEnvironment, String)> {

        // Nodes with module imports need fresh env each time —
        // R6RS library imports are static, so cached env has stale bindings.
        let has_imports = !imports.is_empty();
        let env = if has_imports {
            self.make_env()
        } else {
            cached_env.unwrap_or_else(|| self.make_env())
        };

        // Generate (import ...) statements from structured imports, prepend to code
        let eval_code = if !imports.is_empty() {
            let mut full = String::new();
            for (canvas, module) in imports {
                full.push_str(&format!("(import ({} {}))\n", canvas, module));
            }
            full.push_str(code);
            full
        } else {
            code.to_string()
        };

        // Eval with port context, each top-level form separately
        let (eval_result, port_registry) = eval_with_bridge(
            Some(available_inputs), db,
            || Self::eval_forms(&env, &eval_code),
        );
        let results = eval_result?;
        let render_blocks = extract_render_blocks(&results);

        // Determine output names from exports metadata + widget names
        let widget_decls = port_registry.widgets;
        let mut output_names: Vec<String> = exports.to_vec();
        // Add widget-declared outputs not already in exports
        for w in &widget_decls {
            if !output_names.contains(&w.name) {
                output_names.push(w.name.clone());
            }
        }
        let declared_inputs: Vec<(String, String)> = Vec::new();
        let declared_outputs: Vec<(String, String)> = output_names.iter()
            .map(|n| (n.clone(), "f64".to_string())).collect();

        // Collect output values by name from environment
        let mut output_values = HashMap::new();
        for name in &output_names {
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

        let (tick_interval_ms, ocapn_sends, recompute_requests, has_message_handler, window_title) = collect_side_effects();

        Ok((ScriptResult {
            output_values, render_blocks, declared_inputs, declared_outputs, widget_decls,
            tick_interval_ms, ocapn_sends, recompute_requests, has_message_handler, window_title,
        }, env, eval_code))
    }

    /// Re-eval using code with imports already prepended.
    /// `exports` from Node fields used for output resolution when non-empty.
    pub fn eval_preprocessed(
        &self,
        env: &TopLevelEnvironment,
        available_inputs: &HashMap<String, crate::types::Value>,
        db: Option<&crate::db::Db>,
        preprocessed: &str,
        exports: &[String],
    ) -> Result<ScriptResult> {
        let (eval_result, port_registry) = eval_with_bridge(
            Some(available_inputs), db,
            || Self::eval_forms(env, preprocessed),
        );
        let results = eval_result?;
        let render_blocks = extract_render_blocks(&results);

        let widget_decls = port_registry.widgets;
        let mut output_names: Vec<String> = exports.to_vec();
        for w in &widget_decls {
            if !output_names.contains(&w.name) {
                output_names.push(w.name.clone());
            }
        }
        let declared_inputs: Vec<(String, String)> = Vec::new();
        let declared_outputs: Vec<(String, String)> = output_names.iter()
            .map(|n| (n.clone(), "f64".to_string())).collect();

        let mut output_values = HashMap::new();
        for name in &output_names {
            if let Ok(vals) = env.eval(false, name) {
                if let Some(val) = vals.first() {
                    output_values.insert(name.clone(), scheme_value_to_types_value(val));
                }
            }
        }

        let (tick_interval_ms, ocapn_sends, recompute_requests, has_message_handler, window_title) = collect_side_effects();

        Ok(ScriptResult {
            output_values, render_blocks, declared_inputs, declared_outputs, widget_decls,
            tick_interval_ms, ocapn_sends, recompute_requests, has_message_handler, window_title,
        })
    }

    /// Fast path: drain messages via handler in an existing env, without full re-eval.
    /// Only processes messages and collects side-effects + render output.
    pub fn execute_message_handler(
        &self,
        env: &TopLevelEnvironment,
        available_inputs: &HashMap<String, crate::types::Value>,
        db: Option<&crate::db::Db>,
    ) -> Result<ScriptResult> {
        let (eval_result, _) = eval_with_bridge(
            Some(available_inputs), db,
            || env.eval(false, "(__actor-drain-messages)")
                .map_err(|e| anyhow::anyhow!("{}", e)),
        );
        eval_result?;

        let (tick_interval_ms, ocapn_sends, recompute_requests, _, _) = collect_side_effects();

        Ok(ScriptResult {
            output_values: HashMap::new(), render_blocks: Vec::new(),
            declared_inputs: Vec::new(), declared_outputs: Vec::new(), widget_decls: Vec::new(),
            tick_interval_ms, ocapn_sends, recompute_requests,
            has_message_handler: true, window_title: None,
        })
    }

    /// Preview: bind all inputs as <compute> placeholder, eval for structure only
    pub fn preview_script(
        &self,
        db: Option<&crate::db::Db>,
        code: &str,
    ) -> Result<ScriptResult> {
        let env = self.make_env();

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

        // Eval with port context (None = preview mode, inputs return <compute>)
        let (eval_result, _) = eval_with_bridge(
            None, db,
            || Self::eval_forms(&env, code),
        );
        let results = eval_result?;
        let render_blocks = extract_render_blocks(&results);

        Ok(ScriptResult {
            output_values: HashMap::new(), render_blocks,
            declared_inputs: Vec::new(), declared_outputs: Vec::new(), widget_decls: Vec::new(),
            tick_interval_ms: None, ocapn_sends: Vec::new(), recompute_requests: Vec::new(),
            has_message_handler: false, window_title: None,
        })
    }

}

#[derive(Debug, Clone)]
pub struct ScriptResult {
    pub output_values: HashMap<String, crate::types::Value>,
    pub render_blocks: Vec<RenderBlock>,
    pub declared_inputs: Vec<(String, String)>,
    pub declared_outputs: Vec<(String, String)>,
    pub widget_decls: Vec<crate::bridge::WidgetDecl>,
    pub tick_interval_ms: Option<u64>,
    pub ocapn_sends: Vec<crate::bridge::OCapNSendEntry>,
    pub recompute_requests: Vec<crate::types::NodeId>,
    pub has_message_handler: bool,
    pub window_title: Option<String>,
}

impl ScriptResult {
    pub fn empty() -> Self {
        Self {
            output_values: HashMap::new(),
            render_blocks: Vec::new(),
            declared_inputs: Vec::new(),
            declared_outputs: Vec::new(),
            widget_decls: Vec::new(),

            tick_interval_ms: None,
            ocapn_sends: Vec::new(),
            recompute_requests: Vec::new(),
            has_message_handler: false,
            window_title: None,
        }
    }

    pub fn with_outputs(output_values: HashMap<String, crate::types::Value>) -> Self {
        Self {
            output_values,
            ..Self::empty()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_module_header() {
        let code = r#"
(define-module (my-canvas controls)
  (export gain freq))

(define gain 50.0)
"#;
        let header = parse_module_header(code).unwrap();
        assert_eq!(header.canvas, "my-canvas");
        assert_eq!(header.name, "controls");
        assert_eq!(header.exports, vec!["gain", "freq"]);
        assert!(header.imports.is_empty());

        let code2 = r#"
(define-module (demo wave)
  (use-module (demo controls))
  (use-module (demo gateway-in))
  (export osc))
"#;
        let header2 = parse_module_header(code2).unwrap();
        assert_eq!(header2.canvas, "demo");
        assert_eq!(header2.name, "wave");
        assert_eq!(header2.exports, vec!["osc"]);
        assert_eq!(header2.imports, vec![
            ("demo".to_string(), "controls".to_string()),
            ("demo".to_string(), "gateway-in".to_string()),
        ]);

        // No define-module
        assert!(parse_module_header("(define x 1)").is_none());

        // Cross-canvas imports
        let code3 = r#"
(define-module (my-canvas synth)
  (use-module (my-canvas local-mod))
  (use-module (other-canvas controls))
  (export sound))
"#;
        let header3 = parse_module_header(code3).unwrap();
        assert_eq!(header3.canvas, "my-canvas");
        assert_eq!(header3.name, "synth");
        assert_eq!(header3.imports, vec![
            ("my-canvas".to_string(), "local-mod".to_string()),
            ("other-canvas".to_string(), "controls".to_string()),
        ]);
    }

    #[test]
    fn test_strip_module_header() {
        let code = "(define-module (demo test)\n  (use-module (demo foo))\n  (export bar))\n\n(define bar 42)";
        let (body, imports) = strip_module_header(code);
        assert!(!body.contains("define-module"));
        assert!(body.contains("(define bar 42)"));
        assert_eq!(imports, vec!["(import (demo foo))"]);

        // Cross-canvas strip
        let code2 = "(define-module (demo x)\n  (use-module (my-canvas ctrl))\n  (export y))\n(define y 1)";
        let (_, imports2) = strip_module_header(code2);
        assert!(imports2.contains(&"(import (my-canvas ctrl))".to_string()));
    }

    #[test]
    fn test_extract_imports() {
        let code = "(define-module (demo wave)\n  (use-module (demo controls))\n  (use-module (demo gateway-in))\n  (export osc))";
        let imports = extract_imports(code);
        assert_eq!(imports, vec![
            ("demo".to_string(), "controls".to_string()),
            ("demo".to_string(), "gateway-in".to_string()),
        ]);

        assert!(extract_imports("(define x 1)").is_empty());
    }

    #[test]
    fn test_interleaved_defines() {
        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();
        db.kv_set("gain", serde_json::json!(2.0));
        // define after set!, then use the defined variable
        let exports = vec!["r".to_string()];
        let (result, _, _) = engine.execute_script_cached(
            None,
            &inputs(&[("x", 5.0)]),
            Some(&db),
            "(define x 5)\n(define r 0)\n(set! r (* x 2))\n(define gain (store-get \"gain\"))\n(set! r (* r gain))",
            &exports,
            &[],
        ).unwrap();
        // x=5, r=5*2=10, gain=2, r=10*2=20
        assert!(matches!(result.output_values.get("r"), Some(crate::types::Value::F64(v)) if (*v - 20.0).abs() < 1e-10));
    }

    #[test]
    fn test_define_visible_in_render_block() {
        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();
        db.kv_set("gain", serde_json::json!(2.0));
        // define from store-get, then reference in render call
        let result = engine.execute_script(
            &HashMap::new(),
            Some(&db),
            "(define gain (store-get \"gain\"))\n(render (text (string-append \"Gain is \" (->str gain))))",
        ).unwrap();
        assert!(!result.render_blocks.is_empty());
        let has_gain = result.render_blocks.iter().any(|b|
            matches!(b, RenderBlock::Text(t) if t.contains("2")));
        assert!(has_gain, "Expected render block with gain value, got: {:?}", result.render_blocks);
    }

    #[test]
    fn test_basic_arithmetic() {
        let engine = SchemeEngine::new().unwrap();
        let results = engine.eval("(+ 2 3)").unwrap();
        assert_eq!(results.len(), 1);
        let val = results[0].cast_to_scheme_type::<f64>();
        println!("(+ 2 3) = {:?}", val);
        assert!(val.is_some());
    }

    fn inputs(pairs: &[(&str, f64)]) -> HashMap<String, crate::types::Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), crate::types::Value::F64(*v))).collect()
    }

    #[test]
    fn test_with_bindings() {
        let engine = SchemeEngine::new().unwrap();
        let exports = vec!["result".to_string()];
        let (result, _, _) = engine.execute_script_cached(
            None,
            &inputs(&[("x", 7.0), ("y", 3.0)]),
            None,
            "(define x 7)\n(define y 3)\n(define result (* x y))",
            &exports,
            &[],
        ).unwrap();
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
        let result = engine.execute_script(&HashMap::new(), None, "(bold \"hello world\")").unwrap();
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
                &HashMap::new(),
                None,
                r#"(define x 42)
(render (bold "Result") (text "Value: " (number->string x)))"#,
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
                None,
                r#"(define x <compute>)
(define y <compute>)
(render (bold "Analysis") (text "x + y = " (+ x y)) (hr) (plot-line (list 1 2 x 4) "test"))"#,
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
                &HashMap::new(),
                None,
                r#"(render (bold "Test") (button "Click Me" 'set "key1" "val1"))"#,
            )
            .unwrap();
        println!("button render blocks: {:?}", result.render_blocks);

        let has_button = result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Button { label, .. } if label == "Click Me"));
        assert!(has_button, "Expected a Button with label 'Click Me', got: {:?}", result.render_blocks);
    }

    #[test]
    fn test_splice_button_parsing() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine.execute_script(
            &HashMap::new(),
            None,
            r#"(render (button "Delete" 'splice "todos" "2" "1" ""))"#,
        ).unwrap();
        let has_button = result.render_blocks.iter().any(|b|
            matches!(b, RenderBlock::Button { label, action } if label == "Delete"
                && matches!(action, crate::render::StoreAction::Splice { key, index, delete_count, value }
                    if key == "todos" && *index == 2 && *delete_count == 1 && value.is_empty())));
        assert!(has_button, "Expected Splice button, got: {:?}", result.render_blocks);
    }

    #[test]
    fn test_render_map() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine.execute_script(
            &HashMap::new(),
            None,
            r#"(render-map '("alpha" "beta" "gamma") (lambda (item i) (text (->str i) ". " item)))"#,
        ).unwrap();
        // Should produce 3 text blocks
        assert!(result.render_blocks.len() >= 3,
            "Expected at least 3 render blocks, got: {:?}", result.render_blocks);
        let texts: Vec<&str> = result.render_blocks.iter().filter_map(|b| {
            if let RenderBlock::Text(t) = b { Some(t.as_str()) } else { None }
        }).collect();
        assert!(texts.iter().any(|t| t.contains("alpha")), "Missing alpha in {:?}", texts);
        assert!(texts.iter().any(|t| t.contains("beta")), "Missing beta in {:?}", texts);
        assert!(texts.iter().any(|t| t.contains("gamma")), "Missing gamma in {:?}", texts);
    }

    #[test]
    fn test_render_map_with_buttons() {
        let engine = SchemeEngine::new().unwrap();
        // Simulate a todo list rendering with per-item delete buttons
        let result = engine.execute_script(
            &HashMap::new(),
            None,
            r#"(render-map '("buy milk" "fix bug") (lambda (item i) (render (text item) (button "x" 'splice "todos" (->str i) "1" ""))))"#,
        ).unwrap();
        let button_count = result.render_blocks.iter().filter(|b| matches!(b, RenderBlock::Button { .. })).count();
        assert_eq!(button_count, 2, "Expected 2 delete buttons, got: {:?}", result.render_blocks);
    }

    #[test]
    fn test_bridge_store_roundtrip() {
        use serde_json::json;

        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();

        // store-set! then store-get in same script works via bridge
        let result = engine
            .execute_script(
                &HashMap::new(),
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

        let _result = engine
            .execute_script(
                &HashMap::new(),
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
        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();

        let _result = engine
            .execute_script(
                &HashMap::new(),
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
                &HashMap::new(),
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
        let exports = vec!["msg".to_string()];
        let imports = vec![("mylib".to_string(), String::new())];
        let result = engine.execute_script_cached(
            None,
            &HashMap::new(),
            None,
            "(import (mylib))\n(define msg (greet \"World\"))",
            &exports,
            &[],
        );
        // Clean up
        std::fs::remove_file(&lib_file).ok();
        std::fs::remove_dir(&dir).ok();

        let (result, _, _) = result.unwrap();
        match result.output_values.get("msg") {
            Some(crate::types::Value::Str(s)) => assert!(s.contains("Hello"), "Got: {}", s),
            other => panic!("Expected string output, got: {:?}", other),
        }
    }

    #[test]
    fn test_missing_library_error() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine.execute_script(
            &HashMap::new(),
            None,
            "(import (nonexistent-lib-xyz))",
        );
        assert!(result.is_err(), "Expected error for missing library");
    }

    #[test]
    fn test_widget_as_output() {
        let engine = SchemeEngine::new().unwrap();
        let mut map = HashMap::new();
        map.insert("brightness".to_string(), crate::types::Value::F64(200.0));
        let result = engine.execute_script(&map, None,
            "(define brightness (slider 'brightness 0 255))").unwrap();
        assert_eq!(result.widget_decls.len(), 1);
        assert_eq!(result.widget_decls[0].name, "brightness");
        assert_eq!(result.widget_decls[0].widget_type, "slider");
        assert!(matches!(result.output_values.get("brightness"), Some(crate::types::Value::F64(v)) if (*v - 200.0).abs() < 1e-10));
    }

    #[test]
    fn test_widget_default_value() {
        let engine = SchemeEngine::new().unwrap();
        // No persisted value — should use min (0) as default
        let result = engine.execute_script(&HashMap::new(), None,
            "(define brightness (slider 'brightness 0 255))").unwrap();
        assert!(matches!(result.output_values.get("brightness"), Some(crate::types::Value::F64(v)) if (*v - 0.0).abs() < 1e-10));
    }

    #[test]
    fn test_checkbox_widget() {
        let engine = SchemeEngine::new().unwrap();
        let mut map = HashMap::new();
        map.insert("enabled".to_string(), crate::types::Value::F64(1.0));
        let result = engine.execute_script(&map, None,
            "(define enabled (checkbox 'enabled))").unwrap();
        assert_eq!(result.widget_decls.len(), 1);
        assert_eq!(result.widget_decls[0].widget_type, "checkbox");
        assert!(matches!(result.output_values.get("enabled"), Some(crate::types::Value::F64(v)) if (*v - 1.0).abs() < 1e-10));
    }

    #[test]
    fn test_scheme_render_pipeline() {
        let engine = SchemeEngine::new().unwrap();
        let code = "(define x 5)\n(define result (* x 2))\n(render (bold \"Title\") (text (string-append \"Result is \" (->str result))))";
        let exports = vec!["result".to_string()];
        let (result, _, _) = engine.execute_script_cached(
            None,
            &HashMap::new(),
            None,
            code,
            &exports,
            &[],
        ).unwrap();
        assert!(matches!(result.output_values.get("result"),
            Some(crate::types::Value::F64(v)) if (*v - 10.0).abs() < f64::EPSILON));
        assert!(!result.render_blocks.is_empty());
        assert!(result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Bold(_))));
        assert!(result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Text(t) if t.contains("10"))));
    }

    #[test]
    fn test_canvas_draw_commands() {
        let engine = SchemeEngine::new().unwrap();
        let result = engine
            .execute_script(
                &HashMap::new(),
                None,
                r##"
                (canvas 200 100
                    (draw-rect 0 0 200 100 "#ff0000")
                    (draw-line 0 0 200 100 "#00ff00" 2)
                    (draw-circle 100 50 20 "#0000ff")
                    (draw-text 10 10 "hello" "#000000" 12))
                "##,
            )
            .unwrap();
        println!("canvas blocks: {:?}", result.render_blocks);
        assert!(!result.render_blocks.is_empty());
        assert!(result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Canvas { .. })));
    }

    #[test]
    fn test_canvas_wave_script() {
        let engine = SchemeEngine::new().unwrap();
        let script = r##"
(define gain 50.0)
(define freq 5.0)

(define pi 3.14159265)
(define w 300.0)
(define h 150.0)
(define mid (/ h 2.0))
(define n 60)

(define (wave-points i acc)
  (if (= i n) acc
    (let* ((x (* (/ i n) w))
           (y (+ mid (* gain (sin (* freq (/ i n) pi 4.0))))))
      (wave-points (+ i 1) (cons (list x y) acc)))))

(define pts (reverse (wave-points 0 '())))

(canvas w h
    (draw-rect 0 0 w h "#f5f5f0")
    (draw-line 0 mid w mid "#cccccc" 1)
    (draw-polyline pts "#2266cc" 2)
    (draw-text 4 4 "oscilloscope" "#666666" 12))
        "##;
        let result = engine.execute_script(&HashMap::new(), None, script).unwrap();
        println!("wave blocks: {:?}", result.render_blocks);
        assert!(result.render_blocks.iter().any(|b| matches!(b, RenderBlock::Canvas { .. })));
    }
}
