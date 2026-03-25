use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Result of evaluating code.
pub struct EvalResult {
    pub value: Option<String>,
    pub out: Option<String>,
    pub err: Option<String>,
    pub ex: Option<String>,
}

/// Completion candidate.
pub struct Completion {
    pub candidate: String,
    pub ns: Option<String>,
    pub kind: Option<String>, // "var", "function", "module", "keyword"
}

/// Symbol info for go-to-definition.
pub struct SymbolInfo {
    pub name: String,
    pub ns: Option<String>,
    pub file: Option<String>,
    pub doc: Option<String>,
}

/// Result of load-file operation.
pub struct LoadFileResult {
    pub value: Option<String>,
    pub error: Option<String>,
}

/// Trait for language-specific evaluation. Implement this to plug in any language runtime.
pub trait Evaluator: Send + Sync {
    fn eval(&self, session_id: &str, code: &str) -> EvalResult;

    /// Return completions for a prefix in the given session/namespace.
    fn completions(&self, session_id: &str, prefix: &str, ns: Option<&str>) -> Vec<Completion> {
        let _ = (session_id, prefix, ns);
        Vec::new()
    }

    /// Return info about a symbol (go-to-definition).
    fn info(&self, session_id: &str, symbol: &str, ns: Option<&str>) -> Option<SymbolInfo> {
        let _ = (session_id, symbol, ns);
        None
    }

    /// Load a file by path — find the corresponding node, update code, recompute.
    fn load_file(&self, session_id: &str, file_path: &str, file_content: &str) -> LoadFileResult {
        let _ = (session_id, file_path, file_content);
        LoadFileResult { value: None, error: Some("load-file not implemented".into()) }
    }

    /// List available namespaces (nodes).
    fn ns_list(&self, _session_id: &str) -> Vec<String> {
        Vec::new()
    }

    /// Switch session to a namespace (node context). Returns true if successful.
    fn switch_ns(&self, session_id: &str, ns: &str) -> bool {
        let _ = (session_id, ns);
        false
    }

    /// Create a new node on a canvas. Returns node id or error.
    fn create_node(&self, canvas: &str, label: &str, code: &str,
                   exports: &[String], imports: &[(String, String)]) -> Result<String, String> {
        let _ = (canvas, label, code, exports, imports);
        Err("create-node not implemented".into())
    }

    /// Delete a node by canvas/label.
    fn delete_node(&self, canvas: &str, label: &str) -> Result<(), String> {
        let _ = (canvas, label);
        Err("delete-node not implemented".into())
    }

    /// Update a node's code, exports, and/or imports.
    fn update_node(&self, canvas: &str, label: &str,
                   code: Option<&str>, exports: Option<&[String]>,
                   imports: Option<&[(String, String)]>) -> Result<(), String> {
        let _ = (canvas, label, code, exports, imports);
        Err("update-node not implemented".into())
    }

    /// Read a node's current state: code, exports, imports, outputs, error.
    fn node_state(&self, canvas: &str, label: &str) -> Option<NodeState> {
        let _ = (canvas, label);
        None
    }

    /// Trigger compute for a node. Returns immediately; poll results via node-state.
    fn compute_node(&self, canvas: &str, label: &str) -> Result<(), String> {
        let _ = (canvas, label);
        Err("compute not implemented".into())
    }
}

/// Full state of a node, returned by node-state op.
pub struct NodeState {
    pub code: String,
    pub exports: Vec<String>,
    pub imports: Vec<(String, String)>,
    pub outputs: Vec<(String, String)>, // (name, stringified value)
    pub error: Option<String>,
}

/// A simple evaluator that echoes code back (for testing).
pub struct EchoEvaluator;

impl Evaluator for EchoEvaluator {
    fn eval(&self, _session_id: &str, code: &str) -> EvalResult {
        EvalResult {
            value: Some(code.to_string()),
            out: None,
            err: None,
            ex: None,
        }
    }
}

/// Session state — currently just an ID. Language-specific state lives in the Evaluator.
pub struct Session {
    pub id: String,
}

/// Manages active nREPL sessions.
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Session>>,
    counter: Mutex<u64>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
        }
    }

    pub fn create_session(&self) -> String {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let id = format!("session-{}", *counter);
        let session = Session { id: id.clone() };
        self.sessions.lock().unwrap().insert(id.clone(), session);
        id
    }

    pub fn has_session(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(id)
    }

    pub fn close_session(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().remove(id).is_some()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

/// Dispatch an nREPL message, returning response messages.
pub fn handle_message(
    msg: &crate::bencode::Value,
    sessions: &SessionManager,
    evaluator: &dyn Evaluator,
) -> Vec<crate::bencode::Value> {
    use crate::bencode::Value;

    let op = msg.get_str("op").unwrap_or("");
    let id = msg.get_str("id").unwrap_or("unknown");
    let session_id = msg.get_str("session").unwrap_or("");

    match op {
        "clone" => {
            let new_id = sessions.create_session();
            vec![Value::dict(vec![
                ("id", Value::string(id)),
                ("new-session", Value::string(&new_id)),
                ("status", Value::List(vec![Value::string("done")])),
            ])]
        }
        "close" => {
            sessions.close_session(session_id);
            vec![Value::dict(vec![
                ("id", Value::string(id)),
                ("session", Value::string(session_id)),
                ("status", Value::List(vec![Value::string("done")])),
            ])]
        }
        "describe" => {
            vec![Value::dict(vec![
                ("id", Value::string(id)),
                ("ops", Value::dict(vec![
                    ("clone", Value::dict(vec![])),
                    ("close", Value::dict(vec![])),
                    ("completions", Value::dict(vec![])),
                    ("describe", Value::dict(vec![])),
                    ("eval", Value::dict(vec![])),
                    ("info", Value::dict(vec![])),
                    ("load-file", Value::dict(vec![])),
                    ("ns-list", Value::dict(vec![])),
                    ("switch-ns", Value::dict(vec![])),
                    ("create-node", Value::dict(vec![])),
                    ("delete-node", Value::dict(vec![])),
                    ("update-node", Value::dict(vec![])),
                    ("node-state", Value::dict(vec![])),
                    ("compute", Value::dict(vec![])),
                ])),
                ("status", Value::List(vec![Value::string("done")])),
                ("versions", Value::dict(vec![
                    ("nrepl", Value::dict(vec![
                        ("major", Value::Int(0)),
                        ("minor", Value::Int(1)),
                    ])),
                ])),
            ])]
        }
        "eval" => {
            if !sessions.has_session(session_id) {
                return vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("status", Value::List(vec![
                        Value::string("error"),
                        Value::string("unknown-session"),
                        Value::string("done"),
                    ])),
                ])];
            }

            let code = msg.get_str("code").unwrap_or("");
            let result = evaluator.eval(session_id, code);
            let mut responses = Vec::new();

            if let Some(ref out) = result.out {
                responses.push(Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("out", Value::string(out.as_str())),
                ]));
            }

            if let Some(ref err) = result.err {
                responses.push(Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("err", Value::string(err.as_str())),
                ]));
            }

            let status = if result.ex.is_some() {
                Value::List(vec![Value::string("eval-error"), Value::string("done")])
            } else {
                Value::List(vec![Value::string("done")])
            };

            let mut final_resp = vec![
                ("id", Value::string(id)),
                ("session", Value::string(session_id)),
                ("status", status),
            ];
            if let Some(ref val) = result.value {
                final_resp.push(("value", Value::string(val.as_str())));
            }
            if let Some(ref ex) = result.ex {
                final_resp.push(("ex", Value::string(ex.as_str())));
            }
            responses.push(Value::dict(final_resp));

            responses
        }
        "completions" => {
            let prefix = msg.get_str("prefix").unwrap_or("");
            let ns = msg.get_str("ns");
            let completions = evaluator.completions(session_id, prefix, ns);
            let candidates: Vec<Value> = completions.into_iter().map(|c| {
                let mut pairs = vec![
                    ("candidate", Value::string(&c.candidate)),
                ];
                if let Some(ref ns) = c.ns {
                    pairs.push(("ns", Value::string(ns.as_str())));
                }
                if let Some(ref kind) = c.kind {
                    pairs.push(("type", Value::string(kind.as_str())));
                }
                Value::dict(pairs)
            }).collect();
            vec![Value::dict(vec![
                ("id", Value::string(id)),
                ("session", Value::string(session_id)),
                ("completions", Value::List(candidates)),
                ("status", Value::List(vec![Value::string("done")])),
            ])]
        }
        "info" => {
            let symbol = msg.get_str("symbol").unwrap_or("");
            let ns = msg.get_str("ns");
            let resp = match evaluator.info(session_id, symbol, ns) {
                Some(info) => {
                    let mut pairs = vec![
                        ("id", Value::string(id)),
                        ("session", Value::string(session_id)),
                        ("name", Value::string(&info.name)),
                        ("status", Value::List(vec![Value::string("done")])),
                    ];
                    if let Some(ref ns) = info.ns {
                        pairs.push(("ns", Value::string(ns.as_str())));
                    }
                    if let Some(ref file) = info.file {
                        pairs.push(("file", Value::string(file.as_str())));
                    }
                    if let Some(ref doc) = info.doc {
                        pairs.push(("doc", Value::string(doc.as_str())));
                    }
                    Value::dict(pairs)
                }
                None => Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("status", Value::List(vec![Value::string("no-info"), Value::string("done")])),
                ]),
            };
            vec![resp]
        }
        "load-file" => {
            let file_content = msg.get_str("file").unwrap_or("");
            let file_path = msg.get_str("file-path").unwrap_or("");
            let result = evaluator.load_file(session_id, file_path, file_content);
            let status = if result.error.is_some() {
                Value::List(vec![Value::string("eval-error"), Value::string("done")])
            } else {
                Value::List(vec![Value::string("done")])
            };
            let mut pairs = vec![
                ("id", Value::string(id)),
                ("session", Value::string(session_id)),
                ("status", status),
            ];
            if let Some(ref val) = result.value {
                pairs.push(("value", Value::string(val.as_str())));
            }
            if let Some(ref ex) = result.error {
                pairs.push(("ex", Value::string(ex.as_str())));
            }
            vec![Value::dict(pairs)]
        }
        "ns-list" => {
            let namespaces = evaluator.ns_list(session_id);
            let ns_values: Vec<Value> = namespaces.into_iter().map(|n| Value::string(&n)).collect();
            vec![Value::dict(vec![
                ("id", Value::string(id)),
                ("session", Value::string(session_id)),
                ("ns-list", Value::List(ns_values)),
                ("status", Value::List(vec![Value::string("done")])),
            ])]
        }
        "switch-ns" => {
            let ns = msg.get_str("ns").unwrap_or("");
            let ok = evaluator.switch_ns(session_id, ns);
            let status = if ok {
                Value::List(vec![Value::string("done")])
            } else {
                Value::List(vec![Value::string("error"), Value::string("namespace-not-found"), Value::string("done")])
            };
            vec![Value::dict(vec![
                ("id", Value::string(id)),
                ("session", Value::string(session_id)),
                ("status", status),
            ])]
        }
        "create-node" => {
            let canvas = msg.get_str("canvas").unwrap_or("");
            let label = msg.get_str("label").unwrap_or("");
            let code = msg.get_str("code").unwrap_or("");
            let exports: Vec<String> = msg.get("exports")
                .and_then(|v| v.as_list())
                .map(|l| l.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let imports: Vec<(String, String)> = msg.get("imports")
                .and_then(|v| v.as_list())
                .map(|l| l.iter().filter_map(|v| {
                    let pair = v.as_list()?;
                    if pair.len() == 2 {
                        Some((pair[0].as_str()?.to_string(), pair[1].as_str()?.to_string()))
                    } else { None }
                }).collect())
                .unwrap_or_default();
            match evaluator.create_node(canvas, label, code, &exports, &imports) {
                Ok(node_id) => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("node-id", Value::string(&node_id)),
                    ("status", Value::List(vec![Value::string("done")])),
                ])],
                Err(e) => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("ex", Value::string(&e)),
                    ("status", Value::List(vec![Value::string("error"), Value::string("done")])),
                ])],
            }
        }
        "delete-node" => {
            let canvas = msg.get_str("canvas").unwrap_or("");
            let label = msg.get_str("label").unwrap_or("");
            match evaluator.delete_node(canvas, label) {
                Ok(()) => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("status", Value::List(vec![Value::string("done")])),
                ])],
                Err(e) => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("ex", Value::string(&e)),
                    ("status", Value::List(vec![Value::string("error"), Value::string("done")])),
                ])],
            }
        }
        "update-node" => {
            let canvas = msg.get_str("canvas").unwrap_or("");
            let label = msg.get_str("label").unwrap_or("");
            let code = msg.get_str("code");
            let exports: Option<Vec<String>> = msg.get("exports")
                .and_then(|v| v.as_list())
                .map(|l| l.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect());
            let imports: Option<Vec<(String, String)>> = msg.get("imports")
                .and_then(|v| v.as_list())
                .map(|l| l.iter().filter_map(|v| {
                    let pair = v.as_list()?;
                    if pair.len() == 2 {
                        Some((pair[0].as_str()?.to_string(), pair[1].as_str()?.to_string()))
                    } else { None }
                }).collect());
            match evaluator.update_node(canvas, label, code,
                    exports.as_deref(), imports.as_deref()) {
                Ok(()) => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("status", Value::List(vec![Value::string("done")])),
                ])],
                Err(e) => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("ex", Value::string(&e)),
                    ("status", Value::List(vec![Value::string("error"), Value::string("done")])),
                ])],
            }
        }
        "node-state" => {
            let canvas = msg.get_str("canvas").unwrap_or("");
            let label = msg.get_str("label").unwrap_or("");
            match evaluator.node_state(canvas, label) {
                Some(state) => {
                    let exports_val = Value::List(state.exports.iter().map(|e| Value::string(e)).collect());
                    let imports_val = Value::List(state.imports.iter().map(|(c, m)| {
                        Value::List(vec![Value::string(c), Value::string(m)])
                    }).collect());
                    let outputs_val = Value::dict(
                        state.outputs.iter().map(|(k, v)| (k.as_str(), Value::string(v))).collect()
                    );
                    let mut pairs = vec![
                        ("id", Value::string(id)),
                        ("session", Value::string(session_id)),
                        ("code", Value::string(&state.code)),
                        ("exports", exports_val),
                        ("imports", imports_val),
                        ("outputs", outputs_val),
                        ("status", Value::List(vec![Value::string("done")])),
                    ];
                    if let Some(ref err) = state.error {
                        pairs.push(("error", Value::string(err)));
                    }
                    vec![Value::dict(pairs)]
                }
                None => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("status", Value::List(vec![Value::string("error"), Value::string("no-such-node"), Value::string("done")])),
                ])],
            }
        }
        "compute" => {
            let canvas = msg.get_str("canvas").unwrap_or("");
            let label = msg.get_str("label").unwrap_or("");
            match evaluator.compute_node(canvas, label) {
                Ok(()) => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("status", Value::List(vec![Value::string("done")])),
                ])],
                Err(e) => vec![Value::dict(vec![
                    ("id", Value::string(id)),
                    ("session", Value::string(session_id)),
                    ("ex", Value::string(&e)),
                    ("status", Value::List(vec![Value::string("error"), Value::string("done")])),
                ])],
            }
        }
        _ => {
            vec![Value::dict(vec![
                ("id", Value::string(id)),
                ("session", Value::string(session_id)),
                ("status", Value::List(vec![
                    Value::string("error"),
                    Value::string("unknown-op"),
                    Value::string("done"),
                ])),
            ])]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bencode::Value;

    #[test]
    fn create_session() {
        let mgr = SessionManager::new();
        let id = mgr.create_session();
        assert!(mgr.has_session(&id));
        assert_eq!(mgr.session_count(), 1);
    }

    #[test]
    fn close_session() {
        let mgr = SessionManager::new();
        let id = mgr.create_session();
        assert!(mgr.close_session(&id));
        assert!(!mgr.has_session(&id));
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn eval_in_session() {
        let mgr = SessionManager::new();
        let eval = EchoEvaluator;
        let session = mgr.create_session();
        let msg = Value::dict(vec![
            ("op", Value::string("eval")),
            ("id", Value::string("1")),
            ("session", Value::string(&session)),
            ("code", Value::string("hello")),
        ]);
        let responses = handle_message(&msg, &mgr, &eval);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].get_str("value"), Some("hello"));
    }

    #[test]
    fn eval_unknown_session() {
        let mgr = SessionManager::new();
        let eval = EchoEvaluator;
        let msg = Value::dict(vec![
            ("op", Value::string("eval")),
            ("id", Value::string("1")),
            ("session", Value::string("nonexistent")),
            ("code", Value::string("hello")),
        ]);
        let responses = handle_message(&msg, &mgr, &eval);
        let status = responses[0].get("status").unwrap().as_list().unwrap();
        assert!(status.iter().any(|v| v.as_str() == Some("unknown-session")));
    }

    #[test]
    fn describe_ops() {
        let mgr = SessionManager::new();
        let eval = EchoEvaluator;
        let msg = Value::dict(vec![
            ("op", Value::string("describe")),
            ("id", Value::string("1")),
        ]);
        let responses = handle_message(&msg, &mgr, &eval);
        let ops = responses[0].get("ops").unwrap().as_dict().unwrap();
        assert!(ops.contains_key("eval"));
        assert!(ops.contains_key("clone"));
        assert!(ops.contains_key("close"));
        assert!(ops.contains_key("describe"));
    }
}
