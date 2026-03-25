//! Integration tests: nREPL server backed by SchemeEngine + Db.
//! Tests connect via TCP client — same path as Emacs/CLI would use.

use wasm_canvas::db::Db;
use wasm_canvas::executor::AppResources;
use wasm_canvas::nrepl_eval::SchemeEvaluator;
use nrepl::{Client, Server};
use std::sync::Arc;

/// Spin up a real nREPL server with SchemeEngine + Db, return (server, connected client, session).
fn setup() -> (Server, Client, String) {
    let resources = AppResources::new().expect("Failed to create AppResources");
    let evaluator = Arc::new(SchemeEvaluator::new(
        Arc::clone(&resources.scheme),
        resources.db.clone(),
    ));
    let server = Server::start("127.0.0.1:0", evaluator).unwrap();
    let addr = format!("127.0.0.1:{}", server.port());
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut client = Client::connect(&addr).unwrap();
    let session = client.clone_session().unwrap();
    (server, client, session)
}

/// Helper: eval and return the value string from the last response.
fn eval_value(client: &mut Client, session: &str, code: &str) -> Option<String> {
    let responses = client.eval(session, code).unwrap();
    responses.last().and_then(|r| r.get_str("value").map(|s| s.to_string()))
}

/// Helper: eval and return the ex (exception) string.
fn eval_error(client: &mut Client, session: &str, code: &str) -> Option<String> {
    let responses = client.eval(session, code).unwrap();
    responses.last().and_then(|r| r.get_str("ex").map(|s| s.to_string()))
}

// --- Basic Scheme eval ---

#[test]
fn scheme_eval_arithmetic() {
    let (_server, mut client, session) = setup();
    let val = eval_value(&mut client, &session, "(+ 1 2)");
    assert_eq!(val.as_deref(), Some("3"));
}

#[test]
fn scheme_eval_string() {
    let (_server, mut client, session) = setup();
    let val = eval_value(&mut client, &session, "(string-append \"hello\" \" \" \"world\")");
    assert!(val.unwrap().contains("hello world"));
}

#[test]
fn scheme_eval_define_persist() {
    let (_server, mut client, session) = setup();
    eval_value(&mut client, &session, "(define x 10)");
    let val = eval_value(&mut client, &session, "(* x x)");
    assert_eq!(val.as_deref(), Some("100"));
}

#[test]
fn scheme_eval_multiple_forms() {
    let (_server, mut client, session) = setup();
    // Multiple top-level forms — result is the last one
    let val = eval_value(&mut client, &session, "(define a 3) (define b 4) (+ a b)");
    assert_eq!(val.as_deref(), Some("7"));
}

#[test]
fn scheme_eval_lambda() {
    let (_server, mut client, session) = setup();
    eval_value(&mut client, &session, "(define (square x) (* x x))");
    let val = eval_value(&mut client, &session, "(square 7)");
    assert_eq!(val.as_deref(), Some("49"));
}

#[test]
fn scheme_eval_syntax_error() {
    let (_server, mut client, session) = setup();
    let ex = eval_error(&mut client, &session, "(+ 1");
    assert!(ex.is_some(), "expected eval error for incomplete sexp");
}

#[test]
fn scheme_eval_undefined_var() {
    let (_server, mut client, session) = setup();
    let ex = eval_error(&mut client, &session, "undefined-variable-xyz");
    assert!(ex.is_some(), "expected eval error for undefined variable");
}

// --- Session isolation ---

#[test]
fn sessions_isolated() {
    let (_server, mut client, session1) = setup();
    let session2 = client.clone_session().unwrap();

    // Define x in session1
    eval_value(&mut client, &session1, "(define x 42)");
    let val1 = eval_value(&mut client, &session1, "x");
    assert_eq!(val1.as_deref(), Some("42"));

    // x should not exist in session2
    let ex = eval_error(&mut client, &session2, "x");
    assert!(ex.is_some(), "x should not be defined in session2");
}

// --- KV store ---

#[test]
fn store_set_and_get() {
    let (_server, mut client, session) = setup();
    eval_value(&mut client, &session, "(store-set! \"test-key\" 42)");
    let val = eval_value(&mut client, &session, "(store-get \"test-key\")");
    // Numbers stored in JSON come back as floats
    assert!(val.as_deref().map_or(false, |v| v.contains("42")), "expected 42, got: {:?}", val);
}

#[test]
fn store_get_missing_key() {
    let (_server, mut client, session) = setup();
    // store-get of missing key returns false/#f/null — may render as empty or #f
    let responses = client.eval(&session, "(store-get \"nonexistent-key-abc\")").unwrap();
    let last = responses.last().unwrap();
    // Should not error — either returns a value (false/null) or no value
    let status = last.get("status").unwrap().as_list().unwrap();
    assert!(status.iter().any(|v| v.as_str() == Some("done")));
    assert!(!status.iter().any(|v| v.as_str() == Some("eval-error")),
        "store-get on missing key should not error");
}

#[test]
fn store_delete() {
    let (_server, mut client, session) = setup();
    eval_value(&mut client, &session, "(store-set! \"del-key\" 99)");
    eval_value(&mut client, &session, "(store-delete! \"del-key\")");
    let val = eval_value(&mut client, &session, "(store-get \"del-key\")");
    // After delete, should return null/false, not 99
    assert_ne!(val.as_deref(), Some("99"));
}

#[test]
fn store_keys() {
    let (_server, mut client, session) = setup();
    eval_value(&mut client, &session, "(store-set! \"k1\" 1)");
    eval_value(&mut client, &session, "(store-set! \"k2\" 2)");
    let val = eval_value(&mut client, &session, "(store-keys)");
    let s = val.unwrap();
    assert!(s.contains("k1"), "store-keys should contain k1, got: {}", s);
    assert!(s.contains("k2"), "store-keys should contain k2, got: {}", s);
}

// --- Render DSL availability ---

#[test]
fn render_dsl_available() {
    let (_server, mut client, session) = setup();
    // (hr) is the simplest render function — returns a tagged list
    let responses = client.eval(&session, "(hr)").unwrap();
    let last = responses.last().unwrap();
    let status = last.get("status").unwrap().as_list().unwrap();
    // Should not error — render DSL is loaded
    assert!(!status.iter().any(|v| v.as_str() == Some("eval-error")),
        "render DSL should be available, got: {:?}", last);
}

#[test]
fn canvas_drawing_available() {
    let (_server, mut client, session) = setup();
    let val = eval_value(&mut client, &session, "(draw-line 0 0 10 10 \"#ff0000\" 1)");
    assert!(val.is_some(), "draw-line should be available in REPL");
}

// --- .nrepl-port file ---

#[test]
fn nrepl_port_file_lifecycle() {
    let (server, _client, _session) = setup();
    let dir = std::env::temp_dir().join("nrepl-integration-test");
    let _ = std::fs::remove_dir_all(&dir);

    let path = server.write_port_file(&dir).unwrap();
    assert!(path.exists());

    let content = std::fs::read_to_string(&path).unwrap();
    let port: u16 = content.trim().parse().unwrap();
    assert_eq!(port, server.port());

    // Cleanup
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Concurrent clients ---

#[test]
fn concurrent_clients() {
    let (_server, mut c1, s1) = setup();
    let addr = format!("127.0.0.1:{}", _server.port());
    let mut c2 = Client::connect(&addr).unwrap();
    let s2 = c2.clone_session().unwrap();

    eval_value(&mut c1, &s1, "(define val-c1 111)");
    eval_value(&mut c2, &s2, "(define val-c2 222)");

    let v1 = eval_value(&mut c1, &s1, "val-c1");
    let v2 = eval_value(&mut c2, &s2, "val-c2");
    assert_eq!(v1.as_deref(), Some("111"));
    assert_eq!(v2.as_deref(), Some("222"));

    // Cross-check: c1 should not see c2's define
    let ex = eval_error(&mut c1, &s1, "val-c2");
    assert!(ex.is_some(), "c1 should not see c2's bindings");
}

// --- DB shared across sessions (store is global) ---

#[test]
fn store_shared_across_sessions() {
    let (_server, mut client, s1) = setup();
    let s2 = client.clone_session().unwrap();

    // Set in session1
    eval_value(&mut client, &s1, "(store-set! \"shared-key\" 777)");

    // Read in session2
    let val = eval_value(&mut client, &s2, "(store-get \"shared-key\")");
    assert!(val.as_deref().map_or(false, |v| v.contains("777")), "expected 777, got: {:?}", val);
}
