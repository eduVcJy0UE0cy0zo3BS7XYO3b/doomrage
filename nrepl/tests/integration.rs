use nrepl::{Client, Server, Evaluator, EvalResult};
use std::sync::Arc;

struct TestEvaluator;

impl Evaluator for TestEvaluator {
    fn eval(&self, _session_id: &str, code: &str) -> EvalResult {
        match code.trim() {
            "(+ 1 2)" => EvalResult {
                value: Some("3".into()),
                out: None, err: None, ex: None,
            },
            "(println \"hello\")" => EvalResult {
                value: Some("nil".into()),
                out: Some("hello\n".into()),
                err: None, ex: None,
            },
            "(/ 1 0)" => EvalResult {
                value: None,
                out: None, err: None,
                ex: Some("division by zero".into()),
            },
            _ => EvalResult {
                value: Some(format!("echo: {}", code)),
                out: None, err: None, ex: None,
            },
        }
    }
}

fn start_test_server() -> (Server, String) {
    let server = Server::start("127.0.0.1:0", Arc::new(TestEvaluator)).unwrap();
    let addr = format!("127.0.0.1:{}", server.port());
    // Give server thread a moment to start accepting
    std::thread::sleep(std::time::Duration::from_millis(50));
    (server, addr)
}

#[test]
fn connect_and_clone() {
    let (_server, addr) = start_test_server();
    let mut client = Client::connect(&addr).unwrap();
    let session = client.clone_session().unwrap();
    assert!(session.starts_with("session-"));
}

#[test]
fn eval_simple() {
    let (_server, addr) = start_test_server();
    let mut client = Client::connect(&addr).unwrap();
    let session = client.clone_session().unwrap();
    let responses = client.eval(&session, "(+ 1 2)").unwrap();
    let last = responses.last().unwrap();
    assert_eq!(last.get_str("value"), Some("3"));
}

#[test]
fn eval_with_stdout() {
    let (_server, addr) = start_test_server();
    let mut client = Client::connect(&addr).unwrap();
    let session = client.clone_session().unwrap();
    let responses = client.eval(&session, "(println \"hello\")").unwrap();
    // First message has stdout, last has value + done
    let has_out = responses.iter().any(|r| r.get_str("out") == Some("hello\n"));
    assert!(has_out, "expected stdout message, got: {:?}", responses);
}

#[test]
fn eval_error() {
    let (_server, addr) = start_test_server();
    let mut client = Client::connect(&addr).unwrap();
    let session = client.clone_session().unwrap();
    let responses = client.eval(&session, "(/ 1 0)").unwrap();
    let last = responses.last().unwrap();
    let status = last.get("status").unwrap().as_list().unwrap();
    assert!(status.iter().any(|v| v.as_str() == Some("eval-error")));
    assert_eq!(last.get_str("ex"), Some("division by zero"));
}

#[test]
fn multiple_sessions() {
    let (_server, addr) = start_test_server();
    let mut c1 = Client::connect(&addr).unwrap();
    let mut c2 = Client::connect(&addr).unwrap();
    let s1 = c1.clone_session().unwrap();
    let s2 = c2.clone_session().unwrap();
    assert_ne!(s1, s2);

    let r1 = c1.eval(&s1, "client-1").unwrap();
    let r2 = c2.eval(&s2, "client-2").unwrap();
    assert_eq!(r1.last().unwrap().get_str("value"), Some("echo: client-1"));
    assert_eq!(r2.last().unwrap().get_str("value"), Some("echo: client-2"));
}

#[test]
fn describe() {
    let (_server, addr) = start_test_server();
    let mut client = Client::connect(&addr).unwrap();
    let resp = client.describe().unwrap();
    let ops = resp.get("ops").unwrap().as_dict().unwrap();
    assert!(ops.contains_key("eval"));
    assert!(ops.contains_key("clone"));
}

#[test]
fn port_file() {
    let (server, _addr) = start_test_server();
    let dir = std::env::temp_dir().join("nrepl-test-port");
    let _ = std::fs::remove_dir_all(&dir);

    let path = server.write_port_file(&dir).unwrap();
    assert!(path.exists());

    let content = std::fs::read_to_string(&path).unwrap();
    let port: u16 = content.trim().parse().unwrap();
    assert_eq!(port, server.port());

    let _ = std::fs::remove_dir_all(&dir);
}
