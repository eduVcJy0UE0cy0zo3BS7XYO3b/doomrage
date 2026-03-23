//! Headless daemon: runs wasm-canvas without GUI.
//! Loads graphs from DB, starts P2P network, computes nodes, publishes values.
//!
//! Usage:
//!   wasm-canvas-peer [RELAY_ADDR]
//!
//! Example:
//!   wasm-canvas-peer /ip4/1.2.3.4/tcp/4001/p2p/12D3KooW...

use wasm_canvas::bridge::{NetValues, SharedSessionManager};
use wasm_canvas::executor::AppResources;
use wasm_canvas::network::{NetCommand, NetEvent};
use wasm_canvas::ocapn::session::SessionManager;
use wasm_canvas::persistence;
use wasm_canvas::scheme_engine;
use wasm_canvas::types::*;
use wasm_canvas::actor::{ActorResult, ActorRuntime};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wasm_canvas=info"),
    ).init();

    let relay_addr = std::env::args().nth(1);

    let resources = AppResources::new().expect("Failed to create app resources");

    // Load DB
    let db_path = PathBuf::from("./db.json");
    if let Err(e) = persistence::load_db(&resources.db, &db_path) {
        log::warn!("Failed to restore DB: {}", e);
    }

    // Load canvases
    let canvas_list = persistence::list_canvases(&resources.db);
    let mut all_graphs: HashMap<String, Graph> = HashMap::new();
    for name in &canvas_list {
        if let Ok(Some(g)) = persistence::load_canvas_from_db(name, &resources.db) {
            all_graphs.insert(name.clone(), g);
        } else {
            all_graphs.insert(name.clone(), Graph::new());
        }
    }
    if all_graphs.is_empty() {
        all_graphs.insert("default".to_string(), Graph::new());
    }

    // Init scheme libraries
    for (_, graph) in &all_graphs {
        resources.scheme.register_stub_libraries(&graph.nodes);
    }

    log::info!("Loaded {} canvas(es)", all_graphs.len());
    for (name, g) in &all_graphs {
        log::info!("  {}: {} nodes", name, g.nodes.len());
    }

    // Start network
    let session_manager: SharedSessionManager = Arc::new(Mutex::new(SessionManager::new()));
    let signal: Arc<dyn RepaintSignal> = Arc::new(NoRepaint);
    let net_handle = wasm_canvas::network::spawn_network(signal.clone(), session_manager.clone());
    let net_values: NetValues = Arc::new(Mutex::new(HashMap::new()));

    if let Some(ref addr) = relay_addr {
        log::info!("Connecting to relay: {}", addr);
        net_handle.send(NetCommand::ConnectRelay { addr: addr.clone() });
    }

    // Set up actor runtime
    let mut actor_runtime = ActorRuntime::new(Arc::clone(&resources.scheme));
    actor_runtime.set_net_values(net_values.clone());
    actor_runtime.set_session_mgr(session_manager.clone());
    actor_runtime.set_wasm_runner(resources.wasm.clone());
    actor_runtime.set_repaint_signal(signal);

    let mut pending_nodes: HashSet<NodeId> = HashSet::new();

    // Compute all nodes at startup
    for (_, graph) in &all_graphs {
        if let Ok(order) = graph.topological_sort() {
            for node_id in order {
                if let Some(node) = graph.nodes.get(&node_id) {
                    if node.phantom { continue; }
                    let inputs = graph.resolve_all_input_values(node_id);
                    pending_nodes.insert(node_id);
                    actor_runtime.compute(node_id, node.clone(), None, inputs, resources.db.clone());
                }
            }
        }
    }

    log::info!("Headless daemon running. Ctrl+C to stop.");

    loop {
        // Poll actor results
        while let Some(result) = actor_runtime.poll() {
            match result {
                ActorResult::Computed { node_id, result, .. } => {
                    pending_nodes.remove(&node_id);

                    // Update node
                    for (_, g) in &mut all_graphs {
                        if let Some(n) = g.nodes.get_mut(&node_id) {
                            for (name, val) in &result.output_values {
                                n.output_values.insert(name.clone(), val.clone());
                            }
                            n.error = None;
                            if !result.declared_outputs.is_empty() {
                                n.script_outputs = result.declared_outputs.iter()
                                    .map(|(name, ts)| PortDef {
                                        name: name.clone(),
                                        port_type: PortType::from_str(ts).unwrap_or(PortType::F64),
                                    }).collect();
                            }
                            break;
                        }
                    }

                    // Register library + auto-publish
                    for (_, g) in &all_graphs {
                        if let Some(node) = g.nodes.get(&node_id) {
                            actor_runtime.engine().register_node_library_named(
                                node_id, Some(&node.label), &node.output_values,
                            );
                            if !node.phantom {
                                if let Some(header) = scheme_engine::parse_module_header(&node.script_code) {
                                    if !header.exports.is_empty() && !header.name.is_empty() {
                                        let mut values = node.output_values.clone();
                                        for (k, v) in &node.widget_values { values.insert(k.clone(), v.clone()); }
                                        net_handle.send(NetCommand::Publish { channel: header.name.clone(), values });
                                        log::info!("Published \"{}\"", header.name);
                                    }
                                }
                            }

                            // Propagate downstream
                            let downstream = g.direct_downstream(node_id);
                            for did in downstream {
                                if !pending_nodes.contains(&did) {
                                    if let Some(dn) = g.nodes.get(&did) {
                                        if dn.phantom { continue; }
                                        let inputs = g.resolve_all_input_values(did);
                                        pending_nodes.insert(did);
                                        actor_runtime.compute(did, dn.clone(), None, inputs, resources.db.clone());
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
                ActorResult::Error { node_id, message } => {
                    pending_nodes.remove(&node_id);
                    log::error!("Node #{} error: {}", node_id, message);
                }
            }
        }

        // Poll network
        for event in net_handle.poll() {
            match event {
                NetEvent::PeerDiscovered(peer) => {
                    log::info!("Peer: +{}...", &peer[..12.min(peer.len())]);
                }
                NetEvent::PeerLost(peer) => {
                    log::info!("Peer: -{}...", &peer[..12.min(peer.len())]);
                }
                NetEvent::ValuesReceived { peer, channel, values } => {
                    log::info!("Recv \"{}\": {:?}", channel, values.keys().collect::<Vec<_>>());
                    net_values.lock().unwrap().insert((peer.clone(), channel.clone()), values.clone());

                    for (canvas_key, graph) in &mut all_graphs {
                        let has_local = graph.nodes.values().any(|n| {
                            !n.phantom && scheme_engine::parse_module_header(&n.script_code)
                                .map_or(false, |h| h.name == channel)
                        });
                        if has_local { continue; }

                        // Upsert phantom
                        let phantom_id = graph.nodes.iter()
                            .find(|(_, n)| n.phantom && n.label == channel)
                            .map(|(&id, _)| id);

                        let pid = if let Some(id) = phantom_id {
                            if let Some(n) = graph.nodes.get_mut(&id) {
                                n.output_values = values.clone();
                                n.remote_peer = Some(peer.clone());
                                n.script_outputs = values.keys()
                                    .map(|k| PortDef { name: k.clone(), port_type: PortType::F64 })
                                    .collect();
                            }
                            id
                        } else {
                            let id = graph.next_node_id;
                            graph.next_node_id += 1;
                            let pc = graph.nodes.values().filter(|n| n.phantom).count();
                            graph.nodes.insert(id, Node {
                                id,
                                template_name: "Script".to_string(),
                                label: channel.clone(),
                                pos: [900.0, 50.0 + pc as f32 * 200.0],
                                input_values: HashMap::new(),
                                output_values: values.clone(),
                                script_code: String::new(),
                                script_inputs: Vec::new(),
                                script_outputs: values.keys()
                                    .map(|k| PortDef { name: k.clone(), port_type: PortType::F64 })
                                    .collect(),
                                widget_decls: Vec::new(),
                                widget_values: HashMap::new(),
                                error: None,
                                last_exec_us: None,
                                render_blocks: Vec::new(),
                                phantom: true,
                                remote_peer: Some(peer.clone()),
                            });
                            log::info!("Phantom \"{}\" on \"{}\"", channel, canvas_key);
                            id
                        };

                        actor_runtime.engine().register_node_library_named(pid, Some(&channel), &values);

                        // Recompute downstream
                        let ch = channel.clone();
                        let downstream: Vec<NodeId> = graph.nodes.iter()
                            .filter(|(_, n)| {
                                if n.phantom { return false; }
                                if scheme_engine::extract_imports(&n.script_code).contains(&ch) { return true; }
                                if let Some(h) = scheme_engine::parse_module_header(&n.script_code) {
                                    if h.remote_imports.iter().any(|(_, m)| *m == ch) { return true; }
                                }
                                false
                            })
                            .map(|(id, _)| *id).collect();
                        for did in downstream {
                            if !pending_nodes.contains(&did) {
                                if let Some(n) = graph.nodes.get(&did) {
                                    let inputs = graph.resolve_all_input_values(did);
                                    pending_nodes.insert(did);
                                    actor_runtime.compute(did, n.clone(), None, inputs, resources.db.clone());
                                }
                            }
                        }
                    }
                }
                NetEvent::LocalPeerId(id) => log::info!("PeerId: {}", id),
                _ => {}
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}
