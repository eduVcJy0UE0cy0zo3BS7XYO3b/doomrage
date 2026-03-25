//! Headless daemon: runs wasm-canvas without GUI.
//! Loads graphs from DB, starts P2P network and nREPL server, computes nodes, publishes values.
//!
//! Usage:
//!   wasm-canvas-peer [RELAY_ADDR]
//!
//! Example:
//!   wasm-canvas-peer /ip4/1.2.3.4/tcp/4001/p2p/12D3KooW...

use wasm_canvas::actor::ActorResult;
use wasm_canvas::bridge::NetValues;
use wasm_canvas::executor::AppResources;
use wasm_canvas::graph_runtime::GraphRuntime;
use wasm_canvas::network::{NetCommand, NetEvent};
use wasm_canvas::ocapn::session::SessionManager;
use wasm_canvas::persistence;
use wasm_canvas::types::*;
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

    let user_name = resources.db.kv_get("user_name")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    // Start network
    let session_manager: wasm_canvas::bridge::SharedSessionManager =
        Arc::new(Mutex::new(SessionManager::new()));
    let signal: Arc<dyn RepaintSignal> = Arc::new(NoRepaint);
    let net_handle = wasm_canvas::network::spawn_network(signal.clone(), session_manager.clone());
    let net_values: NetValues = Arc::new(Mutex::new(HashMap::new()));

    if let Some(ref addr) = relay_addr {
        log::info!("Connecting to relay: {}", addr);
        net_handle.send(NetCommand::ConnectRelay { addr: addr.clone() });
    }

    // Build runtime
    let registry = wasm_canvas::registry::NodeRegistry::new(PathBuf::from("./nodes"));
    let mut runtime = GraphRuntime {
        all_graphs,
        actor_runtime: wasm_canvas::actor::ActorRuntime::new(Arc::clone(&resources.scheme)),
        pending_nodes: HashSet::new(),
        net_handle,
        net_values: net_values.clone(),
        user_name,
        registry,
        db: resources.db.clone(),
        peer_names: HashMap::new(),
    };
    runtime.actor_runtime.set_net_values(net_values);
    runtime.actor_runtime.set_session_mgr(session_manager);
    runtime.actor_runtime.set_wasm_runner(resources.wasm.clone());
    runtime.actor_runtime.set_repaint_signal(signal);

    // Init libraries & compute all nodes
    runtime.init_all_libraries();

    log::info!("Loaded {} canvas(es)", runtime.all_graphs.len());
    for (name, g) in &runtime.all_graphs {
        log::info!("  {}: {} nodes", name, g.nodes.len());
    }

    runtime.compute_all();

    // Start nREPL server with command channel
    let (cmd_tx, cmd_rx) = wasm_canvas::nrepl_commands::channel();
    let evaluator = Arc::new(wasm_canvas::nrepl_eval::SchemeEvaluator::new(
        runtime.actor_runtime.engine_arc(),
        resources.db.clone(),
    ));
    evaluator.set_command_sender(cmd_tx);
    unsafe {
        evaluator.set_graphs(
            &runtime.all_graphs as *const HashMap<String, Graph>,
            runtime.all_graphs.keys().next().cloned().unwrap_or_default(),
        );
    }
    let mut nrepl_server = match nrepl::Server::start("127.0.0.1:7888", evaluator) {
        Ok(server) => {
            let port_dir = dirs::home_dir().unwrap_or_default().join(".canvas");
            if let Ok(path) = server.write_port_file(&port_dir) {
                log::info!("nREPL port file: {}", path.display());
            }
            log::info!("nREPL server started on port {}", server.port());
            Some(server)
        }
        Err(e) => {
            log::warn!("Failed to start nREPL server: {}", e);
            None
        }
    };

    // Start file watcher
    let file_watcher = match wasm_canvas::file_watcher::FileWatcher::start() {
        Ok(w) => Some(w),
        Err(e) => {
            log::warn!("Failed to start file watcher: {}", e);
            None
        }
    };

    log::info!("Headless daemon running. Ctrl+C to stop.");

    loop {
        // Poll file watcher
        if let Some(ref watcher) = file_watcher {
            let events = watcher.poll();
            if !events.is_empty() {
                wasm_canvas::file_watcher::apply_file_events(&mut runtime, events);
            }
        }

        // Poll nREPL commands (create-node, delete-node, etc.)
        runtime.poll_nrepl_commands(&cmd_rx);

        // Poll actor results
        while let Some(result) = runtime.actor_runtime.poll() {
            match result {
                ActorResult::Computed { node_id, result, .. } => {
                    runtime.pending_nodes.remove(&node_id);
                    runtime.apply_compute_result(node_id, &result);
                    runtime.register_node_libraries(node_id);
                    runtime.auto_publish_node(node_id);

                    for (peer_id, message) in result.ocapn_sends {
                        runtime.net_handle.send(NetCommand::OCapNSend { peer_id, message });
                    }
                    for target_id in result.recompute_requests {
                        runtime.compute_if_ready(target_id);
                    }

                    runtime.propagate_downstream(node_id);
                }
                ActorResult::Error { node_id, message } => {
                    runtime.pending_nodes.remove(&node_id);
                    log::error!("Node #{} error: {}", node_id, message);
                    if let Some(n) = runtime.find_node_mut(node_id) {
                        n.error = Some(message);
                    }
                }
            }
        }

        // Poll network
        for event in runtime.net_handle.poll() {
            match event {
                NetEvent::PeerDiscovered(peer) => {
                    log::info!("Peer: +{}...", &peer[..12.min(peer.len())]);
                }
                NetEvent::PeerLost(peer) => {
                    log::info!("Peer: -{}...", &peer[..12.min(peer.len())]);
                }
                NetEvent::ValuesReceived { peer, channel, values } => {
                    log::info!("Recv \"{}\": {:?}", channel, values.keys().collect::<Vec<_>>());
                    runtime.net_values.lock().unwrap().insert(
                        (peer.clone(), channel.clone()), values.clone(),
                    );

                    let (source_canvas, module_name) = if let Some(slash) = channel.find('/') {
                        (&channel[..slash], &channel[slash + 1..])
                    } else {
                        (peer.as_str(), channel.as_str())
                    };

                    let canvas_keys: Vec<String> = runtime.all_graphs.keys().cloned().collect();
                    for canvas_key in canvas_keys {
                        runtime.deliver_values(&canvas_key, source_canvas, module_name, &values);
                    }
                }
                NetEvent::LocalPeerId(id) => log::info!("PeerId: {}", id),
                _ => {}
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}
