use crate::render::RenderBlock;
use crate::scheme_convert::try_parse_render_from_value;
pub use crate::scheme_convert::scheme_value_to_json;
use anyhow::Result;
use scheme_rs::env::TopLevelEnvironment;
use scheme_rs::runtime::Runtime;
use scheme_rs::value::{UnpackedValue, Value};
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
          text bold italic code link hr table render
          plot-line plot-scatter plot-bar
          numbered-list bullet-list
          button checkbox text-input slider editable-list
          json-null
          canvas draw-line draw-rect draw-circle draw-polyline draw-text)
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

  ;; Generic canvas drawing
  (define (canvas w h . cmds) (list 'render-canvas w h cmds))
  (define (draw-line x1 y1 x2 y2 color width) (list 'line x1 y1 x2 y2 color width))
  (define (draw-rect x y w h fill) (list 'rect x y w h fill))
  (define (draw-circle x y r fill) (list 'circle x y r fill))
  (define (draw-polyline pts color width) (list 'polyline pts color width))
  (define (draw-text x y txt color size) (list 'text x y txt color size))
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

const CANVAS_SCRIBBLE_LIB: &str = r###"
(library (canvas scribble)
  (export scribble-preprocess)
  (import (rnrs))

  ;; --- String utilities ---

  (define (string-lines str)
    (let ((len (string-length str)))
      (let loop ((i 0) (start 0) (acc '()))
        (cond
          ((= i len)
           (reverse (cons (substring str start len) acc)))
          ((char=? (string-ref str i) #\newline)
           (loop (+ i 1) (+ i 1) (cons (substring str start i) acc)))
          (else (loop (+ i 1) start acc))))))

  (define (string-trim str)
    (let* ((len (string-length str))
           (s (let loop ((i 0))
                (if (and (< i len) (char-whitespace? (string-ref str i)))
                    (loop (+ i 1)) i)))
           (e (let loop ((i len))
                (if (and (> i s) (char-whitespace? (string-ref str (- i 1))))
                    (loop (- i 1)) i))))
      (substring str s e)))

  (define (starts-with? str pre)
    (let ((sl (string-length str)) (pl (string-length pre)))
      (and (>= sl pl) (string=? (substring str 0 pl) pre))))

  (define (ends-with? str suf)
    (let ((sl (string-length str)) (pl (string-length suf)))
      (and (>= sl pl) (string=? (substring str (- sl pl) sl) suf))))

  (define (split-by-char str ch)
    (let ((len (string-length str)))
      (let loop ((i 0) (start 0) (acc '()))
        (cond
          ((= i len) (reverse (cons (substring str start len) acc)))
          ((char=? (string-ref str i) ch)
           (loop (+ i 1) (+ i 1) (cons (substring str start i) acc)))
          (else (loop (+ i 1) start acc))))))

  (define (str-join lst sep)
    (if (null? lst) ""
        (let loop ((rest (cdr lst)) (out (car lst)))
          (if (null? rest) out
              (loop (cdr rest) (string-append out sep (car rest)))))))

  (define (every-char? pred str)
    (let ((len (string-length str)))
      (let loop ((i 0))
        (or (= i len) (and (pred (string-ref str i)) (loop (+ i 1)))))))

  (define (all? pred lst)
    (or (null? lst) (and (pred (car lst)) (all? pred (cdr lst)))))

  (define (escape s)
    (let ((len (string-length s)))
      (let loop ((i 0) (out '()))
        (if (= i len) (list->string (reverse out))
            (let ((c (string-ref s i)))
              (cond
                ((char=? c #\\) (loop (+ i 1) (cons #\\ (cons #\\ out))))
                ((char=? c #\") (loop (+ i 1) (cons #\" (cons #\\ out))))
                (else (loop (+ i 1) (cons c out)))))))))

  (define (ident-char? c)
    (or (char-alphabetic? c) (char-numeric? c)
        (char=? c #\-) (char=? c #\_) (char=? c #\?) (char=? c #\!)))

  ;; --- Inline @-expression processing ---

  (define (process-inline text)
    (let ((len (string-length text)))
      (let loop ((i 0) (s 0) (parts '()))
        (cond
          ((= i len)
           (let ((final (if (< s i)
                            (cons (string-append "\"" (escape (substring text s i)) "\"") parts)
                            parts)))
             (if (null? final) "\"\""
                 (str-join (reverse final) " "))))

          ((and (char=? (string-ref text i) #\@) (< (+ i 1) len))
           (let ((nc (string-ref text (+ i 1))))
             (cond
               ;; @(expr)
               ((char=? nc #\()
                (let ((flushed (if (< s i)
                                   (cons (string-append "\"" (escape (substring text s i)) "\"") parts)
                                   parts)))
                  (let ploop ((j (+ i 1)) (d 0))
                    (cond
                      ((= j len) (loop len len (cons (substring text (+ i 1) j) flushed)))
                      ((char=? (string-ref text j) #\() (ploop (+ j 1) (+ d 1)))
                      ((char=? (string-ref text j) #\))
                       (if (= d 1)
                           (loop (+ j 1) (+ j 1) (cons (substring text (+ i 1) (+ j 1)) flushed))
                           (ploop (+ j 1) (- d 1))))
                      (else (ploop (+ j 1) d))))))

               ;; @name
               ((ident-char? nc)
                (let ((flushed (if (< s i)
                                   (cons (string-append "\"" (escape (substring text s i)) "\"") parts)
                                   parts)))
                  (let nloop ((j (+ i 1)))
                    (if (and (< j len) (ident-char? (string-ref text j)))
                        (nloop (+ j 1))
                        (loop j j (cons (substring text (+ i 1) j) flushed))))))

               ;; bare @
               (else (loop (+ i 1) s parts)))))

          (else (loop (+ i 1) s parts))))))

  ;; --- Table builder ---

  (define (build-table headers rows)
    (string-append
     "(table (list " (str-join (map process-inline headers) " ")
     ") (list " (str-join (map (lambda (row)
                                 (string-append "(list " (str-join (map process-inline row) " ") ")"))
                               rows) " ")
     "))"))

  ;; Flush pending table into render parts
  (define (flush-tbl render hdr rows)
    (if hdr (cons (build-table hdr (reverse rows)) render) render))

  ;; --- Main preprocessor ---

  (define (scribble-preprocess source)
    (let ((lines (string-lines source)))
      (let loop ((ls lines) (scm '()) (ren '()) (hdr #f) (trows '()) (in-r #f))
        (if (null? ls)
            (let ((final-ren (flush-tbl ren hdr trows)))
              (build-output (reverse scm) (reverse final-ren)))

            (let ((t (string-trim (car ls))) (rest (cdr ls)))
              (cond
                ;; blank line
                ((= (string-length t) 0)
                 (loop rest scm (flush-tbl ren hdr trows) #f '() in-r))

                ;; scheme line (starts with paren, not in render mode)
                ((and (char=? (string-ref t 0) #\() (not in-r))
                 (loop rest (cons t scm) (flush-tbl ren hdr trows) #f '() #f))

                ;; hr
                ((or (string=? t "---") (string=? t "***") (string=? t "___"))
                 (loop rest scm (cons "(hr)" (flush-tbl ren hdr trows)) #f '() #t))

                ;; ## heading
                ((starts-with? t "## ")
                 (let ((txt (substring t 3 (string-length t))))
                   (loop rest scm
                         (cons (string-append "(italic " (process-inline txt) ")")
                               (flush-tbl ren hdr trows))
                         #f '() #t)))

                ;; # heading
                ((starts-with? t "# ")
                 (let ((txt (substring t 2 (string-length t))))
                   (loop rest scm
                         (cons (string-append "(bold " (process-inline txt) ")")
                               (flush-tbl ren hdr trows))
                         #f '() #t)))

                ;; table row
                ((and (char=? (string-ref t 0) #\|) (ends-with? t "|"))
                 (let* ((inner (substring t 1 (- (string-length t) 1)))
                        (cells (map string-trim (split-by-char inner #\|))))
                   (if (all? (lambda (c)
                               (every-char? (lambda (ch) (or (char=? ch #\-) (char=? ch #\:))) c))
                             cells)
                       ;; separator row - skip
                       (loop rest scm ren hdr trows #t)
                       ;; data row
                       (if hdr
                           (loop rest scm ren hdr (cons cells trows) #t)
                           (loop rest scm ren cells '() #t)))))

                ;; standalone @(expr)
                ((and (starts-with? t "@(") (ends-with? t ")"))
                 (loop rest scm
                       (cons (substring t 1 (string-length t))
                             (flush-tbl ren hdr trows))
                       #f '() #t))

                ;; plain text
                (else
                 (loop rest scm
                       (cons (string-append "(text " (process-inline t) ")")
                             (flush-tbl ren hdr trows))
                       #f '() #t))))))))

  ;; Build final output
  (define (build-output scm-lines ren-parts)
    (string-append
     (str-join (map (lambda (l) (string-append l "\n")) scm-lines) "")
     (if (null? ren-parts) ""
         (string-append "(render\n"
                        (str-join (map (lambda (p) (string-append "  " p "\n")) ren-parts) "")
                        ")"))))
)
"###;

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
        runtime.def_lib(CANVAS_SCRIBBLE_LIB)
            .map_err(|e| anyhow::anyhow!("Failed to define (canvas scribble): {}", e))?;

        let env = TopLevelEnvironment::new_repl(&runtime);
        env.eval(true, "(import (rnrs))")
            .map_err(|e| anyhow::anyhow!("Failed to import rnrs: {}", e))?;
        env.eval(true, "(import (canvas db))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas db: {}", e))?;
        env.eval(true, "(import (canvas render))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas render: {}", e))?;
        env.eval(true, "(import (canvas ports))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas ports: {}", e))?;
        env.eval(true, "(import (canvas scribble))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas scribble: {}", e))?;
        env.eval(true, "(import (canvas net))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas net: {}", e))?;
        env.eval(true, "(import (canvas timer))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas timer: {}", e))?;
        env.eval(true, "(import (canvas ocapn))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas ocapn: {}", e))?;
        env.eval(true, "(import (canvas actor))")
            .map_err(|e| anyhow::anyhow!("Failed to import canvas actor: {}", e))?;

        // Port declaration wrappers: call bridge functions from (canvas ports)
        env.eval(false, r#"
            (define (input name type) (register-input name type))
            (define (output name type) (register-output name type))
            (define (widget name type . params)
              (register-widget name type
                (if (null? params) 0 (car params))
                (if (or (null? params) (null? (cdr params))) 0 (cadr params))))
            (define (slider name lo hi) (register-widget name 'slider lo hi))
            (define (checkbox name) (register-widget name 'checkbox 0 0))
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
            (define (on-message handler)
              (actor-register-handler)
              (let loop ()
                (let ((msg (receive)))
                  (when msg
                    (handler msg)
                    (loop)))))
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
        env.eval(false, r#"
            (define (input name type) (register-input name type))
            (define (output name type) (register-output name type))
            (define (widget name type . params)
              (register-widget name type
                (if (null? params) 0 (car params))
                (if (or (null? params) (null? (cdr params))) 0 (cadr params))))
            (define (slider name lo hi) (register-widget name 'slider lo hi))
            (define (checkbox name) (register-widget name 'checkbox 0 0))
        "#)
            .map_err(|e| anyhow::anyhow!("Failed to define port wrappers in REPL env: {}", e))?;
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

    /// Call the Scheme preprocessor to convert Scribble markup into pure Scheme.
    fn preprocess_code(&self, env: &TopLevelEnvironment, code: &str) -> Result<String> {
        let code_literal = crate::types::Value::Str(code.to_string()).to_scheme_literal();
        let results = env.eval(false, &format!("(scribble-preprocess {})", code_literal))
            .map_err(|e| anyhow::anyhow!("Preprocessing failed: {}", e))?;
        match results.first() {
            Some(val) => match val.clone().unpack() {
                UnpackedValue::String(s) => Ok(String::from(s)),
                _ => Err(anyhow::anyhow!("Preprocessor returned non-string: {}", val)),
            },
            None => Ok(String::new()),
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

    /// Eval preprocessed code form-by-form.
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
            let r = env
                .eval(is_import, trimmed)
                .map_err(|e| anyhow::anyhow!("Eval failed: {}", e))?;
            results.extend(r);
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
        let (result, _env) = self.execute_script_cached(None, available_inputs, db, code)?;
        Ok(result)
    }

    /// Execute a script, optionally reusing a cached environment.
    /// Returns (result, env) — caller can cache the env for next eval.
    /// When env is reused, `set!` mutations persist between computes.
    pub fn execute_script_cached(
        &self,
        cached_env: Option<TopLevelEnvironment>,
        available_inputs: &HashMap<String, crate::types::Value>,
        db: Option<&crate::db::Db>,
        code: &str,
    ) -> Result<(ScriptResult, TopLevelEnvironment)> {
        let env = cached_env.unwrap_or_else(|| self.make_env());

        let stripped = self.preprocess_code(&env, code)?;

        // Eval with port context, each top-level form separately
        let (eval_result, port_registry) = crate::bridge::with_port_context(
            Some(available_inputs),
            || {
                let eval_fn = || Self::eval_forms(&env, &stripped);
                if let Some(db) = db {
                    crate::bridge::with_db_context(db, eval_fn)
                } else {
                    eval_fn()
                }
            },
        );
        let results = eval_result?;

        let mut render_blocks = Vec::new();
        for val in &results {
            if let Some(blocks) = try_parse_render_from_value(val) {
                render_blocks.extend(blocks);
            }
        }

        let (declared_inputs, declared_outputs, widget_decls) =
            (port_registry.inputs, port_registry.outputs, port_registry.widgets);

        // Collect output values by name from environment
        let output_names: Vec<String> = declared_outputs.iter().map(|(name, _)| name.clone()).collect();
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

        let net_publishes = crate::bridge::take_net_publishes();
        let tick_interval_ms = crate::bridge::take_tick_interval();
        let ocapn_sends = crate::bridge::take_ocapn_sends();
        let recompute_requests = crate::bridge::take_recompute_requests();
        let has_message_handler = crate::bridge::take_has_message_handler();

        Ok((ScriptResult {
            output_values,
            render_blocks,
            declared_inputs,
            declared_outputs,
            widget_decls,
            net_publishes,
            tick_interval_ms,
            ocapn_sends,
            recompute_requests,
            has_message_handler,
        }, env))
    }

    /// Preview: bind all inputs as <compute> placeholder, eval for structure only
    pub fn preview_script(
        &self,
        db: Option<&crate::db::Db>,
        code: &str,
    ) -> Result<ScriptResult> {
        let env = self.make_env();

        let stripped = self.preprocess_code(&env, code)?;

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
        let (eval_result, _port_registry) = crate::bridge::with_port_context(
            None,
            || {
                let eval_fn = || Self::eval_forms(&env, &stripped);
                if let Some(db) = db {
                    crate::bridge::with_db_context(db, eval_fn)
                } else {
                    eval_fn()
                }
            },
        );
        let results = eval_result?;

        let mut render_blocks = Vec::new();
        for val in &results {
            if let Some(blocks) = try_parse_render_from_value(val) {
                render_blocks.extend(blocks);
            }
        }

        Ok(ScriptResult {
            output_values: HashMap::new(),
            render_blocks,
            declared_inputs: Vec::new(),
            declared_outputs: Vec::new(),
            widget_decls: Vec::new(),
            net_publishes: Vec::new(),
            tick_interval_ms: None,
            ocapn_sends: Vec::new(),
            recompute_requests: Vec::new(),
            has_message_handler: false,
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
    pub net_publishes: Vec<String>,
    pub tick_interval_ms: Option<u64>,
    pub ocapn_sends: Vec<crate::bridge::OCapNSendEntry>,
    pub recompute_requests: Vec<crate::types::NodeId>,
    pub has_message_handler: bool,
}

impl ScriptResult {
    pub fn empty() -> Self {
        Self {
            output_values: HashMap::new(),
            render_blocks: Vec::new(),
            declared_inputs: Vec::new(),
            declared_outputs: Vec::new(),
            widget_decls: Vec::new(),
            net_publishes: Vec::new(),
            tick_interval_ms: None,
            ocapn_sends: Vec::new(),
            recompute_requests: Vec::new(),
            has_message_handler: false,
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
    fn test_scribble_preprocess() {
        let engine = SchemeEngine::new().unwrap();
        let env = engine.make_env();
        let output = engine.preprocess_code(&env, r#"(define x (input 'x 'f64))
(define result (output 'result 'f64))

(set! result (* x 2))

# My Analysis

The result is @result.

---

| name | value |
|------|-------|
| x    | @x    |

@(plot-line '(1 2 3) "test")
"#).unwrap();
        println!("--- preprocessed ---\n{}\n---", output);
        assert!(output.contains("(define x (input 'x 'f64))"));
        assert!(output.contains("(define result (output 'result 'f64))"));
        assert!(output.contains("(set! result (* x 2))"));
        assert!(output.contains("(render"));
        assert!(output.contains(r#"(bold "My Analysis")"#));
        assert!(output.contains("result"));
        assert!(output.contains("(hr)"));
        assert!(output.contains("(table"));
        assert!(output.contains("(plot-line '(1 2 3) \"test\")"));
    }

    #[test]
    fn test_scribble_inline_expressions() {
        let engine = SchemeEngine::new().unwrap();
        let env = engine.make_env();

        // Test inline @name
        let output = engine.preprocess_code(&env, "hello @name world").unwrap();
        assert!(output.contains(r#""hello " name " world""#));

        // Test inline @(expr)
        let output = engine.preprocess_code(&env, "sum = @(+ x y)!").unwrap();
        assert!(output.contains(r#""sum = " (+ x y) "!""#));

        // Test plain text
        let output = engine.preprocess_code(&env, "no at signs").unwrap();
        assert!(output.contains(r#""no at signs""#));
    }

    #[test]
    fn test_interleaved_defines() {
        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();
        db.kv_set("gain", serde_json::json!(2.0));
        // define after set!, then use the defined variable
        let result = engine.execute_script(
            &inputs(&[("x", 5.0)]),
            Some(&db),
            "(define x (input 'x 'f64))\n(define r (output 'r 'f64))\n(set! r (* x 2))\n(define gain (store-get \"gain\"))\n(set! r (* r gain))",
        ).unwrap();
        // x=5, r=5*2=10, gain=2, r=10*2=20
        assert!(matches!(result.output_values.get("r"), Some(crate::types::Value::F64(v)) if (*v - 20.0).abs() < 1e-10));
    }

    #[test]
    fn test_define_visible_in_render_block() {
        let engine = SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();
        db.kv_set("gain", serde_json::json!(2.0));
        // define from store-get, then reference in Scribble render block
        let result = engine.execute_script(
            &HashMap::new(),
            Some(&db),
            "(define gain (store-get \"gain\"))\n\n# Result\n\nGain is @gain.",
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
        let map = inputs(&[("x", 7.0), ("y", 3.0)]);
        let result = engine.execute_script(&map, None, "(define x (input 'x 'f64))\n(define y (input 'y 'f64))\n(define result (output 'result 'f64))\n(set! result (* x y))").unwrap();
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
                &inputs(&[("x", 42.0)]),
                None,
                r#"(define x (input 'x 'f64))
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
                r#"(define x (input 'x 'f64))
(define y (input 'y 'f64))
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
        let result = engine.execute_script(
            &HashMap::new(),
            None,
            "(import (mylib))\n(define msg (output 'msg 'str))\n(set! msg (greet \"World\"))",
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
            &HashMap::new(),
            None,
            "(import (nonexistent-lib-xyz))",
        );
        assert!(result.is_err(), "Expected error for missing library");
    }

    #[test]
    fn test_dynamic_input_port() {
        let engine = SchemeEngine::new().unwrap();
        let mut map = HashMap::new();
        map.insert("x".to_string(), crate::types::Value::F64(7.0));
        let result = engine.execute_script(&map, None,
            "(define x (input 'x 'f64))\n(define r (output 'r 'f64))\n(set! r (* x 3))").unwrap();
        assert_eq!(result.declared_inputs.len(), 1);
        assert_eq!(result.declared_inputs[0].0, "x");
        assert_eq!(result.declared_outputs.len(), 1);
        assert_eq!(result.declared_outputs[0].0, "r");
        assert!(matches!(result.output_values.get("r"), Some(crate::types::Value::F64(v)) if (*v - 21.0).abs() < 1e-10));
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
    fn test_new_syntax_input_returns_value() {
        let engine = SchemeEngine::new().unwrap();
        let map = inputs(&[("a", 3.0), ("b", 4.0)]);
        let result = engine.execute_script(&map, None,
            "(define a (input 'a 'f64))\n(define b (input 'b 'f64))\n(define r (output 'r 'f64))\n(set! r (+ a b))").unwrap();
        assert!(matches!(result.output_values.get("r"), Some(crate::types::Value::F64(v)) if (*v - 7.0).abs() < 1e-10));
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
    fn test_scribble_to_render_pipeline() {
        let engine = SchemeEngine::new().unwrap();
        let code = "(define x (input 'x 'f64))\n(define result (output 'result 'f64))\n(set! result (* x 2))\n\n# Title\n\nResult is @result.";
        let result = engine.execute_script(
            &inputs(&[("x", 5.0)]),
            None,
            code,
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
(define gain-raw (input 'gain 'f64))
(define freq-raw (input 'freq 'f64))
(define gain (if (compute? gain-raw) 50.0 gain-raw))
(define freq (if (compute? freq-raw) 5.0 freq-raw))

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
