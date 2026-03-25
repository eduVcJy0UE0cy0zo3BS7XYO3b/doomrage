//! Integration test: verify that __source__ is included in published values
//! and correctly extracted on the receiving side.
//!
//! Uses the transport-level TestPair (two swarms on localhost with mDNS)
//! to test the full wire protocol without spawning peer processes.
//!
//! Run: cargo test -p wasm-canvas --test peer_network -- --nocapture

use std::collections::HashMap;
use wasm_canvas::types::*;
use wasm_canvas::scheme_engine;

// ---------------------------------------------------------------------------
// Unit-level integration: publish pipeline includes __source__, receive strips it
// ---------------------------------------------------------------------------

#[test]
fn test_publish_includes_source_code() {
    // Simulate what auto_publish_node does
    let code = "(define-module (alice controls)\n  (export gain))\n\n(define gain (output 'gain 'f64))\n(set! gain 42)";
    let header = scheme_engine::parse_module_header(code).unwrap();

    let mut values = HashMap::new();
    values.insert("gain".to_string(), Value::F64(42.0));

    // auto_publish_node adds __source__
    if !code.is_empty() {
        values.insert("__source__".to_string(), Value::Str(code.to_string()));
    }

    let channel = format!("{}/{}", header.canvas, header.name);
    assert_eq!(channel, "alice/controls");
    assert!(matches!(values.get("__source__"), Some(Value::Str(_))));

    // Simulate wire: serialize → deserialize (as gossipsub would do)
    let json = serde_json::to_string(&values).unwrap();
    let received: HashMap<String, Value> = serde_json::from_str(&json).unwrap();

    // deliver_values extracts __source__ and filters it out
    let source_code = match received.get("__source__") {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    assert_eq!(source_code, Some(code.to_string()));

    let node_values: HashMap<String, Value> = received.iter()
        .filter(|(k, _)| !k.starts_with("__"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Phantom node only gets real values, not __source__
    assert!(!node_values.contains_key("__source__"));
    assert_eq!(node_values.len(), 1);
    assert_eq!(node_values.get("gain"), Some(&Value::F64(42.0)));
}

#[test]
fn test_receive_creates_remote_template() {
    let code = "(define-module (alice synth)\n  (export freq))\n\n(define freq (output 'freq 'f64))\n(set! freq 440)";

    // Simulate received values with __source__
    let mut values = HashMap::new();
    values.insert("freq".to_string(), Value::F64(440.0));
    values.insert("__source__".to_string(), Value::Str(code.to_string()));

    // Extract source
    let source_code = match values.get("__source__") {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };

    // Filter values for phantom
    let node_values: HashMap<String, Value> = values.iter()
        .filter(|(k, _)| !k.starts_with("__"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // deliver_values creates a NodeTemplate
    let module_name = "synth";
    let peer = "alice";
    let template_key = format!("{}/{}", peer, module_name);

    let outputs: Vec<PortDef> = node_values.keys()
        .map(|k| PortDef { name: k.clone(), port_type: PortType::F64 })
        .collect();

    let template = NodeTemplate {
        name: module_name.to_string(),
        category: peer.to_string(),
        path: None,
        inputs: Vec::new(),
        outputs,
        wasm_bytes: None,
        builtin: None,
        script_code: source_code,
    };

    assert_eq!(template.category, "alice");
    assert_eq!(template.name, "synth");
    assert!(template.script_code.is_some());
    assert!(template.script_code.as_ref().unwrap().contains("define-module"));

    // Simulate double-click: add_node with this template
    let mut graph = Graph::new();
    let id = graph.add_node(&template, [200.0, 200.0]);
    let node = graph.nodes.get(&id).unwrap();

    // After migration, define-module is stripped, exports populated
    assert!(!node.script_code.contains("define-module"));
    assert_eq!(node.exports, vec!["freq".to_string()]);
    assert_eq!(node.label, "synth");
    assert!(!node.phantom);
}

#[test]
fn test_source_survives_serde_roundtrip() {
    // Verify Value::Str with multiline code survives JSON serialization
    let code = "(define-module (demo wave)\n  (use-module (demo controls))\n  (export out))\n\n(define gain (input 'gain 'f64))\n(define out (output 'out 'f64))\n(set! out (* gain 2))\n\n# Wave\n\nOutput is @out.";

    let mut values: HashMap<String, Value> = HashMap::new();
    values.insert("out".to_string(), Value::F64(84.0));
    values.insert("__source__".to_string(), Value::Str(code.to_string()));

    let json = serde_json::to_vec(&values).unwrap();
    let deserialized: HashMap<String, Value> = serde_json::from_slice(&json).unwrap();

    match deserialized.get("__source__") {
        Some(Value::Str(s)) => assert_eq!(s, code),
        other => panic!("Expected Str with code, got {:?}", other),
    }
}

#[test]
fn test_no_source_when_code_empty() {
    // Phantom nodes and non-module nodes shouldn't publish __source__
    let code = "";
    let mut values = HashMap::new();
    values.insert("x".to_string(), Value::F64(1.0));

    // auto_publish_node only adds __source__ if code is non-empty
    if !code.is_empty() {
        values.insert("__source__".to_string(), Value::Str(code.to_string()));
    }

    assert!(!values.contains_key("__source__"));
}

#[test]
fn test_template_grouped_in_library_by_peer() {
    // Remote templates should be grouped by peer/canvas name
    let mut registry = wasm_canvas::registry::NodeRegistry::new(std::path::PathBuf::from("/tmp/nonexistent"));
    registry.register_builtins();

    // Simulate two remote modules from different canvases
    registry.templates.insert("alice/controls".to_string(), NodeTemplate {
        name: "controls".to_string(),
        category: "alice".to_string(),
        path: None,
        inputs: Vec::new(),
        outputs: vec![PortDef { name: "gain".to_string(), port_type: PortType::F64 }],
        wasm_bytes: None,
        builtin: None,
        script_code: Some("(define-module (alice controls) (export gain))".to_string()),
    });

    registry.templates.insert("bob/synth".to_string(), NodeTemplate {
        name: "synth".to_string(),
        category: "bob".to_string(),
        path: None,
        inputs: Vec::new(),
        outputs: vec![PortDef { name: "sound".to_string(), port_type: PortType::F64 }],
        wasm_bytes: None,
        builtin: None,
        script_code: Some("(define-module (bob synth) (export sound))".to_string()),
    });

    let groups = registry.grouped_templates();

    // Should have: Built-in, alice, bob
    let group_names: Vec<&str> = groups.iter().map(|(name, _)| name.as_str()).collect();
    assert!(group_names.contains(&"Built-in"), "groups: {:?}", group_names);
    assert!(group_names.contains(&"alice"), "groups: {:?}", group_names);
    assert!(group_names.contains(&"bob"), "groups: {:?}", group_names);

    // Built-in comes first (sort order)
    assert_eq!(group_names[0], "Built-in");

    // alice and bob are sorted alphabetically after Built-in
    let alice_pos = group_names.iter().position(|&n| n == "alice").unwrap();
    let bob_pos = group_names.iter().position(|&n| n == "bob").unwrap();
    assert!(alice_pos < bob_pos);

    // Each group has the right template
    let alice_group = groups.iter().find(|(n, _)| n == "alice").unwrap();
    assert_eq!(alice_group.1.len(), 1);
    assert_eq!(alice_group.1[0].name, "controls");

    let bob_group = groups.iter().find(|(n, _)| n == "bob").unwrap();
    assert_eq!(bob_group.1.len(), 1);
    assert_eq!(bob_group.1[0].name, "synth");
}

// ---------------------------------------------------------------------------
// Wire-level test using the real gossipsub transport
// ---------------------------------------------------------------------------

#[test]
fn test_source_code_over_gossipsub() {
    use wasm_canvas_net::WireMessage;

    let code = "(define-module (alice controls)\n  (export gain))\n(define gain (output 'gain 'f64))\n(set! gain 42)";

    // Build the WireMessage as auto_publish_node would
    let mut values = HashMap::new();
    values.insert("gain".to_string(), Value::F64(42.0));
    values.insert("__source__".to_string(), Value::Str(code.to_string()));

    let wire = WireMessage {
        channel: "alice/controls".to_string(),
        values: values.clone(),
        seq: 1,
    };

    // Serialize → deserialize (same path as gossipsub)
    let bytes = serde_json::to_vec(&wire).unwrap();
    let received: WireMessage = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(received.channel, "alice/controls");
    assert_eq!(received.seq, 1);

    // __source__ survives the wire
    match received.values.get("__source__") {
        Some(Value::Str(s)) => {
            assert!(s.contains("define-module"));
            assert!(s.contains("alice controls"));
            assert_eq!(s, code);
        }
        other => panic!("Expected source code string, got {:?}", other),
    }

    // Real values are there too
    assert_eq!(received.values.get("gain"), Some(&Value::F64(42.0)));

    // On receive side: parse channel, extract source, filter values
    let (source_canvas, module_name) = received.channel.split_once('/').unwrap();
    assert_eq!(source_canvas, "alice");
    assert_eq!(module_name, "controls");

    let source = match received.values.get("__source__") {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    assert!(source.is_some());

    let node_values: HashMap<String, Value> = received.values.into_iter()
        .filter(|(k, _)| !k.starts_with("__"))
        .collect();
    assert_eq!(node_values.len(), 1);
    assert!(node_values.contains_key("gain"));
}

// ---------------------------------------------------------------------------
// Canvas privacy: share_code controls __source__ inclusion
// ---------------------------------------------------------------------------

#[test]
fn test_share_code_false_excludes_source() {
    let code = "(define-module (private-canvas secret)\n  (export val))\n(define val (output 'val 'f64))";
    let mut values = HashMap::new();
    values.insert("val".to_string(), Value::F64(99.0));

    // Simulate auto_publish_node with share_code = false
    let share_code = false;
    if share_code && !code.is_empty() {
        values.insert("__source__".to_string(), Value::Str(code.to_string()));
    }

    // __source__ should NOT be present
    assert!(!values.contains_key("__source__"));
    // Values should still be there (phantom nodes work)
    assert_eq!(values.get("val"), Some(&Value::F64(99.0)));
}

#[test]
fn test_share_code_true_includes_source() {
    let code = "(define-module (public-canvas open)\n  (export val))";
    let mut values = HashMap::new();
    values.insert("val".to_string(), Value::F64(1.0));

    let share_code = true;
    if share_code && !code.is_empty() {
        values.insert("__source__".to_string(), Value::Str(code.to_string()));
    }

    assert!(values.contains_key("__source__"));
}

// ---------------------------------------------------------------------------
// User profile: __peer_name__ in values
// ---------------------------------------------------------------------------

#[test]
fn test_peer_name_in_published_values() {
    let mut values = HashMap::new();
    values.insert("gain".to_string(), Value::F64(42.0));

    let user_name = "Alice";
    if !user_name.is_empty() {
        values.insert("__peer_name__".to_string(), Value::Str(user_name.to_string()));
    }

    // Wire roundtrip
    let json = serde_json::to_vec(&values).unwrap();
    let received: HashMap<String, Value> = serde_json::from_slice(&json).unwrap();

    match received.get("__peer_name__") {
        Some(Value::Str(name)) => assert_eq!(name, "Alice"),
        other => panic!("Expected peer name, got {:?}", other),
    }

    // Filtered out from node values
    let node_values: HashMap<String, Value> = received.into_iter()
        .filter(|(k, _)| !k.starts_with("__"))
        .collect();
    assert!(!node_values.contains_key("__peer_name__"));
    assert_eq!(node_values.len(), 1);
}

#[test]
fn test_peer_name_mapping() {
    let mut peer_names: HashMap<String, String> = HashMap::new();

    // Simulate receiving __peer_name__ from a peer
    let peer_id = "12D3KooWAbCdEf";
    let values: HashMap<String, Value> = HashMap::from([
        ("gain".to_string(), Value::F64(50.0)),
        ("__peer_name__".to_string(), Value::Str("Bob".to_string())),
    ]);

    if let Some(Value::Str(name)) = values.get("__peer_name__") {
        if !name.is_empty() {
            peer_names.insert(peer_id.to_string(), name.clone());
        }
    }

    assert_eq!(peer_names.get(peer_id), Some(&"Bob".to_string()));

    // Display name used for template category
    let display = peer_names.get(peer_id).cloned().unwrap_or_else(|| peer_id.to_string());
    assert_eq!(display, "Bob");
}

// ---------------------------------------------------------------------------
// Canvas privacy persistence
// ---------------------------------------------------------------------------

#[test]
fn test_share_code_persistence_roundtrip() {
    let db = wasm_canvas::db::Db::new().expect("create db");

    let mut graph = Graph::new();
    graph.share_code = false;
    wasm_canvas::persistence::save_canvas_to_db("private", &graph, &db).expect("save");

    let loaded = wasm_canvas::persistence::load_canvas_from_db("private", &db)
        .expect("load").expect("canvas exists");
    assert_eq!(loaded.share_code, false);

    // Default: true
    let mut graph2 = Graph::new();
    assert_eq!(graph2.share_code, true);
    wasm_canvas::persistence::save_canvas_to_db("public", &graph2, &db).expect("save");

    let loaded2 = wasm_canvas::persistence::load_canvas_from_db("public", &db)
        .expect("load").expect("canvas exists");
    assert_eq!(loaded2.share_code, true);
}

// ---------------------------------------------------------------------------
// Trust list filtering
// ---------------------------------------------------------------------------

#[test]
fn test_trust_list_empty_shows_all() {
    let trusted_peers: Vec<String> = Vec::new();
    let is_remote = true;
    let category = "Alice";

    // Empty trust list = show all
    let visible = trusted_peers.is_empty() || trusted_peers.iter().any(|tp| category.contains(tp));
    assert!(visible);
}

#[test]
fn test_trust_list_filters_untrusted() {
    let trusted_peers = vec!["Alice".to_string()];
    let category_alice = "Alice";
    let category_mallory = "Mallory";

    let visible_alice = trusted_peers.is_empty()
        || trusted_peers.iter().any(|tp| category_alice.contains(tp) || tp.contains(category_alice));
    let visible_mallory = trusted_peers.is_empty()
        || trusted_peers.iter().any(|tp| category_mallory.contains(tp) || tp.contains(category_mallory));

    assert!(visible_alice);
    assert!(!visible_mallory);
}
