use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Result of evaluating code.
pub struct EvalResult {
    pub value: Option<String>,
    pub out: Option<String>,
    pub err: Option<String>,
    pub ex: Option<String>,
}

/// Trait for language-specific evaluation. Implement this to plug in any language runtime.
pub trait Evaluator: Send + Sync {
    fn eval(&self, session_id: &str, code: &str) -> EvalResult;
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
                    ("describe", Value::dict(vec![])),
                    ("eval", Value::dict(vec![])),
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
