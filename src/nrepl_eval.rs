use crate::scheme_engine::SchemeEngine;
use crate::db::Db;
use nrepl::{Evaluator, EvalResult};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use scheme_rs::env::TopLevelEnvironment;

/// nREPL Evaluator backed by SchemeEngine.
/// Each session gets a persistent REPL environment.
pub struct SchemeEvaluator {
    engine: Arc<SchemeEngine>,
    db: Db,
    sessions: Mutex<HashMap<String, TopLevelEnvironment>>,
}

impl SchemeEvaluator {
    pub fn new(engine: Arc<SchemeEngine>, db: Db) -> Self {
        Self {
            engine,
            db,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn get_or_create_env(&self, session_id: &str) -> TopLevelEnvironment {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(env) = sessions.get(session_id) {
            return env.clone();
        }
        let env = match self.engine.make_repl_env() {
            Ok(env) => env,
            Err(e) => {
                log::warn!("Failed to create REPL env: {}", e);
                self.engine.make_env()
            }
        };
        sessions.insert(session_id.to_string(), env.clone());
        env
    }
}

impl Evaluator for SchemeEvaluator {
    fn eval(&self, session_id: &str, code: &str) -> EvalResult {
        let env = self.get_or_create_env(session_id);

        // Eval with DB context for store-get/store-set! access
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
}
