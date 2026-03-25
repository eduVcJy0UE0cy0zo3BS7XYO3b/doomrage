use crate::scheme_engine::SchemeEngine;
use crate::db::Db;
use crate::persistence;
use crate::types::*;
use nrepl::{Evaluator, EvalResult, Completion, SymbolInfo, LoadFileResult};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use scheme_rs::env::TopLevelEnvironment;

/// Session state: env + optional node context.
struct SessionState {
    env: TopLevelEnvironment,
    /// If set, eval runs in context of this node (sees its imports).
    node_ns: Option<String>, // canvas/label format
}

/// nREPL Evaluator backed by SchemeEngine.
/// Each session gets a persistent REPL environment.
pub struct SchemeEvaluator {
    engine: Arc<SchemeEngine>,
    db: Db,
    sessions: Mutex<HashMap<String, SessionState>>,
    /// Shared access to the graph for ns-list, completions, info, load-file.
    /// Set from app/peer after construction via set_graphs().
    graphs: Arc<RwLock<Option<GraphsRef>>>,
}

/// Shared read access to all graphs + current canvas name.
pub struct GraphsRef {
    pub all_graphs: *const HashMap<String, Graph>,
    pub canvas_name: String,
}

// SAFETY: GraphsRef is read-only and only accessed within RwLock guard.
unsafe impl Send for GraphsRef {}
unsafe impl Sync for GraphsRef {}

/// Built-in symbols available in every REPL session.
const BUILTIN_SYMBOLS: &[(&str, &str)] = &[
    // Render DSL
    ("text", "function"), ("bold", "function"), ("italic", "function"),
    ("code", "function"), ("link", "function"), ("hr", "function"),
    ("table", "function"), ("render", "function"), ("row", "function"),
    ("group", "function"), ("render-map", "function"),
    ("plot-line", "function"), ("plot-scatter", "function"), ("plot-bar", "function"),
    ("canvas", "function"), ("draw-line", "function"), ("draw-rect", "function"),
    ("draw-circle", "function"), ("draw-polyline", "function"), ("draw-text", "function"),
    ("button", "function"), ("checkbox", "function"), ("text-input", "function"),
    ("slider", "function"), ("editable-list", "function"),
    ("interactive", "function"), ("on", "function"),
    ("node-view", "function"), ("node-blocks", "function"), ("node-widgets", "function"),
    // DB
    ("store-get", "function"), ("store-set!", "function"), ("store-delete!", "function"),
    ("store-append!", "function"), ("store-keys", "function"),
    ("db-query", "function"), ("db-run", "function"),
    // Ports / Widgets
    ("widget", "function"),
    // Graph
    ("make-node", "function"), ("connect", "function"), ("remove-node", "function"),
    ("list-nodes", "function"), ("node-ref", "function"), ("node-set!", "function"),
    // Utils
    ("->str", "function"), ("compute?", "function"),
];

impl SchemeEvaluator {
    pub fn new(engine: Arc<SchemeEngine>, db: Db) -> Self {
        Self {
            engine,
            db,
            sessions: Mutex::new(HashMap::new()),
            graphs: Arc::new(RwLock::new(None)),
        }
    }

    /// Set shared graph reference for ns-list, completions, info.
    /// SAFETY: caller must ensure the pointer remains valid while evaluator is alive.
    pub unsafe fn set_graphs(&self, all_graphs: *const HashMap<String, Graph>, canvas_name: String) {
        let mut guard = self.graphs.write().unwrap();
        *guard = Some(GraphsRef { all_graphs, canvas_name });
    }

    fn get_or_create_session(&self, session_id: &str) -> TopLevelEnvironment {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get(session_id) {
            return state.env.clone();
        }
        let env = match self.engine.make_repl_env() {
            Ok(env) => env,
            Err(e) => {
                log::warn!("Failed to create REPL env: {}", e);
                self.engine.make_env()
            }
        };
        sessions.insert(session_id.to_string(), SessionState {
            env: env.clone(),
            node_ns: None,
        });
        env
    }

    /// Read graphs safely. Returns None if not set.
    fn with_graphs<R>(&self, f: impl FnOnce(&HashMap<String, Graph>, &str) -> R) -> Option<R> {
        let guard = self.graphs.read().unwrap();
        guard.as_ref().map(|gr| {
            let graphs = unsafe { &*gr.all_graphs };
            f(graphs, &gr.canvas_name)
        })
    }

    /// Get the current node context (canvas, label) for a session.
    fn session_ns(&self, session_id: &str) -> Option<String> {
        self.sessions.lock().unwrap()
            .get(session_id)
            .and_then(|s| s.node_ns.clone())
    }

    /// Collect all available symbol names for a session.
    fn available_symbols(&self, session_id: &str) -> Vec<(String, String, Option<String>)> {
        let mut symbols: Vec<(String, String, Option<String>)> = Vec::new();

        // Builtins
        for (name, kind) in BUILTIN_SYMBOLS {
            symbols.push((name.to_string(), kind.to_string(), None));
        }

        // From node imports (if in a node context)
        if let Some(ns) = self.session_ns(session_id) {
            if let Some(parts) = ns.split_once('/') {
                let (canvas, label) = parts;
                self.with_graphs(|graphs, _| {
                    if let Some(graph) = graphs.get(canvas) {
                        let module_name = label.replace(' ', "-");
                        if let Some(node) = graph.nodes.values().find(|n| n.label.replace(' ', "-") == module_name) {
                            // Exports from imported nodes
                            for (_, imp_module) in &node.imports {
                                if let Some(src) = graph.nodes.values().find(|n| n.label.replace(' ', "-") == *imp_module) {
                                    for exp in &src.exports {
                                        symbols.push((exp.clone(), "var".into(), Some(imp_module.clone())));
                                    }
                                }
                            }
                        }
                    }
                });
            }
        }

        symbols
    }
}

impl Evaluator for SchemeEvaluator {
    fn eval(&self, session_id: &str, code: &str) -> EvalResult {
        let env = self.get_or_create_session(session_id);

        let result = crate::bridge::with_port_context(
            None,
            || {
                crate::bridge::with_db_context(
                    &self.db,
                    || env.eval(false, code),
                )
            },
        );

        match result.0 {
            Ok(values) => {
                let value_str = values.iter()
                    .map(|v| format!("{}", v))
                    .collect::<Vec<_>>()
                    .join("\n");
                EvalResult {
                    value: if value_str.is_empty() { None } else { Some(value_str) },
                    out: None,
                    err: None,
                    ex: None,
                }
            }
            Err(e) => {
                EvalResult {
                    value: None,
                    out: None,
                    err: None,
                    ex: Some(format!("{}", e)),
                }
            }
        }
    }

    fn completions(&self, session_id: &str, prefix: &str, _ns: Option<&str>) -> Vec<Completion> {
        let symbols = self.available_symbols(session_id);
        symbols.into_iter()
            .filter(|(name, _, _)| name.starts_with(prefix))
            .map(|(name, kind, ns)| Completion {
                candidate: name,
                ns,
                kind: Some(kind),
            })
            .collect()
    }

    fn info(&self, session_id: &str, symbol: &str, _ns: Option<&str>) -> Option<SymbolInfo> {
        // Check builtins
        for (name, kind) in BUILTIN_SYMBOLS {
            if *name == symbol {
                return Some(SymbolInfo {
                    name: name.to_string(),
                    ns: Some("canvas".into()),
                    file: None,
                    doc: Some(format!("Built-in {} from canvas libraries", kind)),
                });
            }
        }

        // Check if symbol comes from an imported node
        if let Some(ns) = self.session_ns(session_id) {
            if let Some(parts) = ns.split_once('/') {
                let (canvas, label) = parts;
                let result = self.with_graphs(|graphs, _| {
                    let graph = graphs.get(canvas)?;
                    let module_name = label.replace(' ', "-");
                    let node = graph.nodes.values().find(|n| n.label.replace(' ', "-") == module_name)?;
                    for (_, imp_module) in &node.imports {
                        if let Some(src) = graph.nodes.values().find(|n| n.label.replace(' ', "-") == *imp_module) {
                            if src.exports.contains(&symbol.to_string()) {
                                let file = persistence::node_file_path(canvas, &src.label);
                                return Some(SymbolInfo {
                                    name: symbol.to_string(),
                                    ns: Some(imp_module.clone()),
                                    file: Some(file.to_string_lossy().into_owned()),
                                    doc: Some(format!("Exported from node \"{}\"", src.label)),
                                });
                            }
                        }
                    }
                    None
                });
                if let Some(Some(info)) = result {
                    return Some(info);
                }
            }
        }

        None
    }

    fn load_file(&self, session_id: &str, file_path: &str, file_content: &str) -> LoadFileResult {
        // Try to find node by file path
        let node_path = std::path::Path::new(file_path);
        let found = self.with_graphs(|graphs, default_canvas| {
            // Try to match file path to a node
            for (canvas_name, graph) in graphs {
                for node in graph.nodes.values() {
                    let expected = persistence::node_file_path(canvas_name, &node.label);
                    if expected == node_path {
                        return Some((canvas_name.clone(), node.id, node.label.clone()));
                    }
                }
            }
            // Fallback: derive from path structure ~/.canvas/nodes/{canvas}/{label}.scm
            if let (Some(label_osstr), Some(canvas_dir)) = (node_path.file_stem(), node_path.parent()) {
                if let (Some(label), Some(canvas_name)) = (label_osstr.to_str(), canvas_dir.file_name().and_then(|f| f.to_str())) {
                    if let Some(graph) = graphs.get(canvas_name) {
                        if let Some(node) = graph.nodes.values().find(|n| n.label.replace(' ', "-") == label) {
                            return Some((canvas_name.to_string(), node.id, node.label.clone()));
                        }
                    }
                }
            }
            None
        });

        match found {
            Some(Some((_canvas, _node_id, label))) => {
                // Write file content to disk
                let _ = std::fs::write(file_path, file_content);
                // Eval the content in the session to give immediate feedback
                let result = self.eval(session_id, file_content);
                LoadFileResult {
                    value: result.value.or(Some(format!("Loaded {}", label))),
                    error: result.ex,
                }
            }
            _ => {
                // Unknown file — just eval the content
                let result = self.eval(session_id, file_content);
                LoadFileResult {
                    value: result.value,
                    error: result.ex,
                }
            }
        }
    }

    fn ns_list(&self, _session_id: &str) -> Vec<String> {
        self.with_graphs(|graphs, _| {
            let mut namespaces = Vec::new();
            for (canvas_name, graph) in graphs {
                for node in graph.nodes.values() {
                    if !node.phantom && node.template_name == "Script" {
                        namespaces.push(format!("{}/{}", canvas_name, node.label.replace(' ', "-")));
                    }
                }
            }
            namespaces.sort();
            namespaces
        }).unwrap_or_default()
    }

    fn switch_ns(&self, session_id: &str, ns: &str) -> bool {
        // Verify namespace exists
        let exists = self.with_graphs(|graphs, _| {
            if let Some(parts) = ns.split_once('/') {
                let (canvas, label) = parts;
                if let Some(graph) = graphs.get(canvas) {
                    return graph.nodes.values().any(|n| n.label.replace(' ', "-") == label);
                }
            }
            false
        }).unwrap_or(false);

        if exists {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(state) = sessions.get_mut(session_id) {
                state.node_ns = Some(ns.to_string());

                // Load imports into the environment
                if let Some(parts) = ns.split_once('/') {
                    let (canvas, label) = parts;
                    self.with_graphs(|graphs, _| {
                        if let Some(graph) = graphs.get(canvas) {
                            let module_name = label.replace(' ', "-");
                            if let Some(node) = graph.nodes.values().find(|n| n.label.replace(' ', "-") == module_name) {
                                // Generate import statements and eval them
                                for (imp_canvas, imp_module) in &node.imports {
                                    let import_stmt = format!("(import ({} {}))", imp_canvas, imp_module);
                                    let _ = state.env.eval(true, &import_stmt);
                                }
                            }
                        }
                    });
                }

                return true;
            }
        }
        false
    }
}
