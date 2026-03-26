//! Headless daemon: runs wasm-canvas without GUI.
//! Loads graphs from DB, starts P2P network and nREPL server, computes nodes, publishes values.
//!
//! Usage:
//!   wasm-canvas-peer --init <dir>              Create a new project
//!   wasm-canvas-peer --project <dir>           Run in a project directory
//!   wasm-canvas-peer [RELAY_ADDR]              Run in current directory (legacy)

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

    let args: Vec<String> = std::env::args().collect();

    // Parse CLI flags
    let mut relay_addr = None;
    let mut project_dir = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--init" => {
                let dir = args.get(i + 1).expect("--init requires a directory argument");
                let path = PathBuf::from(dir);
                persistence::init_project(&path).expect("Failed to init project");
                println!("Project initialized at {}", path.display());
                return;
            }
            "--project" => {
                let dir = args.get(i + 1).expect("--project requires a directory argument");
                project_dir = Some(PathBuf::from(dir));
                i += 2;
            }
            arg if arg.starts_with('/') => {
                relay_addr = Some(arg.to_string());
                i += 1;
            }
            _ => { i += 1; }
        }
    }

    // Set project directory
    if let Some(ref dir) = project_dir {
        persistence::set_project_dir(dir.join(".canvas"));
    }

    let resources = AppResources::new().expect("Failed to create app resources");

    // Load DB
    if let Err(e) = persistence::load_db(&resources.db, &persistence::db_path()) {
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
    let mut registry = wasm_canvas::registry::NodeRegistry::new(PathBuf::from("./nodes"));
    registry.register_builtins();
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
    runtime.request_missing_defs();

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
    // Metrics callback for nREPL
    let nrepl_metrics: std::sync::Arc<nrepl::MetricsCallback> = std::sync::Arc::new(Box::new(|op: &str, dur: std::time::Duration| {
        wasm_canvas::metrics::NREPL_REQUESTS.with_label_values(&[op]).inc();
        wasm_canvas::metrics::NREPL_DURATION.observe(dur.as_secs_f64());
    }));
    let mut nrepl_server = match nrepl::Server::start_with_metrics("127.0.0.1:7888", evaluator, Some(nrepl_metrics)) {
        Ok(server) => {
            let port_dir = persistence::project_dir();
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

    // Start Prometheus metrics HTTP server
    std::thread::spawn(|| {
        let server = match tiny_http::Server::http("0.0.0.0:9090") {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Failed to start metrics server: {}", e);
                return;
            }
        };
        log::info!("Metrics server on http://0.0.0.0:9090/metrics");
        for request in server.incoming_requests() {
            if request.url() == "/metrics" {
                let body = wasm_canvas::metrics::gather();
                let resp = tiny_http::Response::from_string(body)
                    .with_header(tiny_http::Header::from_bytes(
                        &b"Content-Type"[..],
                        &b"text/plain; version=0.0.4; charset=utf-8"[..],
                    ).unwrap());
                let _ = request.respond(resp);
            } else {
                let _ = request.respond(tiny_http::Response::from_string("Not Found").with_status_code(404));
            }
        }
    });

    // Metrics JSONL recording
    let metrics_file = persistence::project_dir().join("metrics.jsonl");
    let mut metrics_writer = std::fs::OpenOptions::new()
        .create(true).append(true).open(&metrics_file).ok();
    if metrics_writer.is_some() {
        log::info!("Recording metrics to {}", metrics_file.display());
    }

    log::info!("Headless daemon running. Ctrl+C to stop.");
    let mut last_gauge_update = std::time::Instant::now();

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
                    wasm_canvas::metrics::PEERS_CONNECTED.inc();
                    runtime.request_missing_defs();
                }
                NetEvent::PeerLost(peer) => {
                    log::info!("Peer: -{}...", &peer[..12.min(peer.len())]);
                    wasm_canvas::metrics::PEERS_CONNECTED.dec();
                }
                NetEvent::ValuesReceived { peer, channel, values } => {
                    // Handle definition request/response channels
                    if runtime.handle_def_network_message(&channel, &values) {
                        continue;
                    }

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

        // Update gauge metrics + write JSONL snapshot periodically
        if last_gauge_update.elapsed() > Duration::from_secs(2) {
            wasm_canvas::metrics::update_gauges(&runtime);
            if let Some(ref mut w) = metrics_writer {
                use std::io::Write;
                let line = wasm_canvas::metrics::snapshot_json();
                let _ = writeln!(w, "{}", line);
                let _ = w.flush();
            }
            last_gauge_update = std::time::Instant::now();
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}
