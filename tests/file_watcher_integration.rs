//! Integration tests for file watcher: edit .scm files → nodes update automatically.

use wasm_canvas::file_watcher::{FileWatcher, FileEvent, apply_file_events};
use wasm_canvas::executor::AppResources;
use wasm_canvas::graph_runtime::GraphRuntime;
use wasm_canvas::actor::ActorRuntime;
use wasm_canvas::registry::NodeRegistry;
use wasm_canvas::types::*;
use wasm_canvas::persistence;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

fn make_test_runtime(nodes_dir: &std::path::Path) -> (GraphRuntime, AppResources) {
    let resources = AppResources::new().expect("Failed to create AppResources");

    let mut graph = Graph::new();
    let mut node = Node {
        id: 1,
        template_name: "Script".to_string(),
        label: "test-node".to_string(),
        pos: [0.0, 0.0],
        input_values: HashMap::new(),
        output_values: HashMap::new(),
        script_code: "(define x 1)".to_string(),
        script_inputs: Vec::new(),
        script_outputs: Vec::new(),
        widget_decls: Vec::new(),
        widget_values: HashMap::new(),
        exports: vec!["x".to_string()],
        imports: Vec::new(),
        hash_imports: Vec::new(),
        definitions: Vec::new(), code_hash: 0,
        error: None,
        last_exec_us: None,
        render_blocks: Vec::new(),
        phantom: false,
        remote_peer: None,
    };
    node.recompute_hash();
    graph.nodes.insert(1, node);
    graph.next_node_id = 2;

    let mut all_graphs = HashMap::new();
    all_graphs.insert("testcanvas".to_string(), graph);

    // Write initial .scm file
    let canvas_dir = nodes_dir.join("testcanvas");
    std::fs::create_dir_all(&canvas_dir).unwrap();
    std::fs::write(canvas_dir.join("test-node.scm"), "(define x 1)").unwrap();

    let net_handle = wasm_canvas::network::spawn_network(
        Arc::new(NoRepaint),
        Arc::new(Mutex::new(wasm_canvas::ocapn::session::SessionManager::new())),
    );

    let mut actor_runtime = ActorRuntime::new(Arc::clone(&resources.scheme));
    actor_runtime.set_wasm_runner(resources.wasm.clone());

    let mut registry = NodeRegistry::new(PathBuf::from("./nodes"));
    registry.templates.insert("Script".to_string(), NodeTemplate {
        name: "Script".to_string(),
        category: "Built-in".to_string(),
        path: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        wasm_bytes: None,
        builtin: Some(BuiltinKind::Script),
        script_code: None,
    });

    let runtime = GraphRuntime {
        all_graphs,
        actor_runtime,
        pending_nodes: HashSet::new(),
        net_handle,
        net_values: Arc::new(Mutex::new(HashMap::new())),
        user_name: String::new(),
        registry,
        db: resources.db.clone(),
        peer_names: HashMap::new(),
    };

    (runtime, resources)
}

#[test]
fn watcher_detects_file_change() {
    let tmp = tempfile::tempdir().unwrap();
    let nodes_dir = tmp.path().to_path_buf();

    let (mut runtime, _resources) = make_test_runtime(&nodes_dir);

    // Start watcher on temp dir
    let watcher = FileWatcher::watch_dir(nodes_dir.clone()).unwrap();

    // Verify initial state
    let node = &runtime.all_graphs["testcanvas"].nodes[&1];
    assert_eq!(node.script_code, "(define x 1)");
    let old_hash = node.code_hash;

    // Modify the file
    std::thread::sleep(std::time::Duration::from_millis(100));
    let file_path = nodes_dir.join("testcanvas").join("test-node.scm");
    std::fs::write(&file_path, "(define x 42)").unwrap();

    // Wait for debounce (200ms) + extra margin
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Poll events
    let events = watcher.poll();
    assert!(!events.is_empty(), "expected file change event");

    // Apply events
    apply_file_events(&mut runtime, events);

    // Verify node updated
    let node = &runtime.all_graphs["testcanvas"].nodes[&1];
    assert_eq!(node.script_code, "(define x 42)");
    assert_ne!(node.code_hash, old_hash);
}

#[test]
fn watcher_ignores_non_scm_files() {
    let tmp = tempfile::tempdir().unwrap();
    let nodes_dir = tmp.path().to_path_buf();
    let canvas_dir = nodes_dir.join("testcanvas");
    std::fs::create_dir_all(&canvas_dir).unwrap();

    let watcher = FileWatcher::watch_dir(nodes_dir).unwrap();

    // Write a non-.scm file
    std::thread::sleep(std::time::Duration::from_millis(100));
    std::fs::write(canvas_dir.join("notes.txt"), "hello").unwrap();

    std::thread::sleep(std::time::Duration::from_millis(500));

    let events = watcher.poll();
    assert!(events.is_empty(), "should ignore non-.scm files, got {} events", events.len());
}

#[test]
fn watcher_unchanged_code_no_recompute() {
    let tmp = tempfile::tempdir().unwrap();
    let nodes_dir = tmp.path().to_path_buf();

    let (mut runtime, _resources) = make_test_runtime(&nodes_dir);

    let watcher = FileWatcher::watch_dir(nodes_dir.clone()).unwrap();

    // Write same content (no actual change)
    std::thread::sleep(std::time::Duration::from_millis(100));
    let file_path = nodes_dir.join("testcanvas").join("test-node.scm");
    std::fs::write(&file_path, "(define x 1)").unwrap(); // same as original

    std::thread::sleep(std::time::Duration::from_millis(500));

    let events = watcher.poll();
    // Events may fire (file touched) but apply_file_events should skip (same hash)
    let pending_before = runtime.pending_nodes.len();
    apply_file_events(&mut runtime, events);
    assert_eq!(runtime.pending_nodes.len(), pending_before,
        "should not recompute when code unchanged");
}

#[test]
fn apply_file_event_unknown_canvas_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let nodes_dir = tmp.path().to_path_buf();
    let (mut runtime, _resources) = make_test_runtime(&nodes_dir);

    let events = vec![FileEvent::NodeChanged {
        canvas: "nonexistent".to_string(),
        label: "test-node".to_string(),
        code: "(define x 99)".to_string(),
    }];

    // Should not panic
    apply_file_events(&mut runtime, events);

    // Original node unchanged
    let node = &runtime.all_graphs["testcanvas"].nodes[&1];
    assert_eq!(node.script_code, "(define x 1)");
}

#[test]
fn apply_file_event_unknown_node_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let nodes_dir = tmp.path().to_path_buf();
    let (mut runtime, _resources) = make_test_runtime(&nodes_dir);

    let events = vec![FileEvent::NodeChanged {
        canvas: "testcanvas".to_string(),
        label: "nonexistent-node".to_string(),
        code: "(define x 99)".to_string(),
    }];

    apply_file_events(&mut runtime, events);

    // Original node unchanged
    let node = &runtime.all_graphs["testcanvas"].nodes[&1];
    assert_eq!(node.script_code, "(define x 1)");
}

/// End-to-end: file change -> watcher -> recompute -> output_values updated.
#[test]
fn full_cycle_file_change_recomputes_output() {
    let tmp = tempfile::tempdir().unwrap();
    let nodes_dir = tmp.path().to_path_buf();
    let (mut runtime, _resources) = make_test_runtime(&nodes_dir);

    // Initial compute
    {
        let graph = runtime.all_graphs.get("testcanvas").unwrap();
        let node = graph.nodes.get(&1).unwrap();
        let inputs = graph.resolve_all_input_values(1);
        let script_template = NodeTemplate {
            name: "Script".to_string(),
            category: "Built-in".to_string(),
            path: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            wasm_bytes: None,
            builtin: Some(BuiltinKind::Script),
            script_code: None,
        };
        runtime.actor_runtime.compute(
            1, node.clone(), Some(script_template), inputs, std::collections::HashMap::new(), runtime.db.clone(),
        );
    }

    // Wait for compute to finish
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(result) = runtime.actor_runtime.poll() {
            match &result {
                wasm_canvas::actor::ActorResult::Computed { result, .. } => {
                    runtime.apply_compute_result(1, result);
                }
                wasm_canvas::actor::ActorResult::Error { message, .. } => {
                    panic!("initial compute error: {}", message);
                }
            }
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("initial compute timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Verify initial output — x should be in outputs
    let node = &runtime.all_graphs["testcanvas"].nodes[&1];
    match node.output_values.get("x") {
        Some(Value::F64(v)) => assert!((*v - 1.0).abs() < f64::EPSILON, "expected x=1, got {}", v),
        other => panic!("expected F64(1), got {:?}. All outputs: {:?}", other, node.output_values),
    }

    // Start watcher
    let watcher = FileWatcher::watch_dir(nodes_dir.clone()).unwrap();

    // Modify file: x = 1 -> x = 42
    std::thread::sleep(std::time::Duration::from_millis(100));
    let file_path = nodes_dir.join("testcanvas").join("test-node.scm");
    std::fs::write(&file_path, "(define x 42)").unwrap();

    // Wait for debounce
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Apply file events (this triggers recompute)
    let events = watcher.poll();
    assert!(!events.is_empty(), "expected file change event");
    apply_file_events(&mut runtime, events);

    // Wait for recompute to finish
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(result) = runtime.actor_runtime.poll() {
            match &result {
                wasm_canvas::actor::ActorResult::Computed { result, .. } => {
                    runtime.apply_compute_result(1, result);
                }
                wasm_canvas::actor::ActorResult::Error { message, .. } => {
                    panic!("recompute error: {}", message);
                }
            }
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("recompute after file change timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Verify output updated
    let node = &runtime.all_graphs["testcanvas"].nodes[&1];
    assert_eq!(node.script_code, "(define x 42)");
    match node.output_values.get("x") {
        Some(Value::F64(v)) => assert!((*v - 42.0).abs() < f64::EPSILON, "expected x=42, got {}", v),
        other => panic!("expected F64(42), got {:?}", other),
    }
}
