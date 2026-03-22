use crate::bridge::{NetValues, OCapNSlotStore, SharedSessionManager};
use crate::canvas::{self, CanvasState};
use crate::debug_log::DebugLog;
use crate::executor::AppResources;
use crate::network::{self, NetCommand, NetEvent, NetHandle};
use crate::ocapn::session::SessionManager;
use crate::panels::{self, PanelAction, PanelState};
use crate::persistence::{self, UndoHistory};
use crate::registry::NodeRegistry;
use crate::theme;
use crate::types::*;
use crate::actor::{ActorResult, ActorRuntime};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct WasmCanvasApp {
    graph: Graph,
    registry: NodeRegistry,
    resources: AppResources,
    actor_runtime: ActorRuntime,
    canvas_state: CanvasState,
    panel_state: PanelState,
    undo_history: UndoHistory,
    current_file: Option<PathBuf>,
    theme_applied: bool,
    pending_nodes: HashSet<NodeId>,
    debug_log: DebugLog,
    net_handle: NetHandle,
    net_values: NetValues,
    /// Nodes requesting periodic ticks: node_id -> (interval_ms, last_tick)
    tick_nodes: HashMap<NodeId, (u64, Instant)>,
    session_manager: SharedSessionManager,
    ocapn_slots: OCapNSlotStore,
    connected_peers: crate::bridge::ConnectedPeers,
    ocapn_call_results: crate::bridge::OCapNCallResults,
    node_mailboxes: crate::bridge::NodeMailboxes,
    ocapn_slot_owners: crate::bridge::OCapNSlotOwners,
    /// Nodes that registered an on-message handler (reactive actors)
    actor_nodes: HashSet<NodeId>,
    /// Nodes with open native windows: node_id → window title
    node_windows: HashMap<NodeId, String>,
    /// Windows the user explicitly closed (don't reopen on recompute)
    closed_windows: HashSet<NodeId>,
    /// Nodes the user explicitly triggered Compute on (for open-window gating)
    explicit_compute: HashSet<NodeId>,
}

impl WasmCanvasApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let nodes_dir = PathBuf::from("./nodes");
        let mut registry = NodeRegistry::new(nodes_dir);
        if let Err(e) = registry.scan() {
            log::error!("Failed to scan nodes directory: {}", e);
        }

        let resources = AppResources::new().expect("Failed to create app resources");

        // Restore DB state from previous session
        let db_auto_path = PathBuf::from("./db.json");
        if let Err(e) = persistence::load_db(&resources.db, &db_auto_path) {
            log::warn!("Failed to restore DB: {}", e);
        }

        // Try loading demo graph on first launch (prefer .scm over .json)
        let (mut graph, current_file) = {
            let scm_path = PathBuf::from("./demo.scm");
            let json_path = PathBuf::from("./demo.json");
            if scm_path.exists() {
                match persistence::load_graph_scm(&scm_path, &resources.db) {
                    Ok(g) => {
                        log::info!("Loaded demo graph from .scm");
                        (g, Some(scm_path))
                    }
                    Err(e) => {
                        log::warn!("Failed to load demo.scm: {}", e);
                        (Graph::new(), None)
                    }
                }
            } else if json_path.exists() {
                match persistence::load_graph(&json_path, &resources.db) {
                    Ok(g) => {
                        log::info!("Loaded demo graph from .json");
                        (g, Some(json_path))
                    }
                    Err(e) => {
                        log::warn!("Failed to load demo.json: {}", e);
                        (Graph::new(), None)
                    }
                }
            } else {
                (Graph::new(), None)
            }
        };
        // Initialize ports and libraries from define-module headers (before compute)
        {
            // 1. Set script_outputs from (export ...) in define-module
            for node in graph.nodes.values_mut() {
                if let Some(header) = crate::scheme_engine::parse_module_header(&node.script_code) {
                    node.script_outputs = header.exports.iter()
                        .map(|name| PortDef { name: name.clone(), port_type: PortType::F64 })
                        .collect();
                }
            }
            // 2. Derive script_inputs from imported modules' exports
            let input_map: HashMap<NodeId, Vec<PortDef>> = graph.nodes.iter()
                .filter_map(|(&id, node)| {
                    let header = crate::scheme_engine::parse_module_header(&node.script_code)?;
                    let mut inputs = Vec::new();
                    for import_label in &header.imports {
                        if let Some(src_id) = graph.find_node_by_import_label(import_label) {
                            if let Some(src) = graph.nodes.get(&src_id) {
                                for port in &src.script_outputs {
                                    if !inputs.iter().any(|p: &PortDef| p.name == port.name) {
                                        inputs.push(port.clone());
                                    }
                                }
                            }
                        }
                    }
                    if inputs.is_empty() { None } else { Some((id, inputs)) }
                })
                .collect();
            for (id, inputs) in input_map {
                if let Some(node) = graph.nodes.get_mut(&id) {
                    node.script_inputs = inputs;
                }
            }
            // 3. Register stub libraries
            resources.scheme.register_stub_libraries(&graph.nodes);
        }

        let mut undo_history = UndoHistory::new(10);
        undo_history.push(&graph);

        let session_manager: SharedSessionManager = Arc::new(Mutex::new(SessionManager::new()));
        let net_handle = network::spawn_network(cc.egui_ctx.clone(), session_manager.clone());
        let net_values: NetValues = Arc::new(Mutex::new(HashMap::new()));
        let ocapn_slots: OCapNSlotStore = Arc::new(Mutex::new(HashMap::new()));
        let connected_peers: crate::bridge::ConnectedPeers = Arc::new(Mutex::new(HashSet::new()));
        let ocapn_call_results: crate::bridge::OCapNCallResults = Arc::new(Mutex::new(HashMap::new()));
        let node_mailboxes: crate::bridge::NodeMailboxes = Arc::new(Mutex::new(HashMap::new()));
        let ocapn_slot_owners: crate::bridge::OCapNSlotOwners = Arc::new(Mutex::new(HashMap::new()));

        let mut actor_runtime = ActorRuntime::new(Arc::clone(&resources.scheme));
        actor_runtime.set_net_values(net_values.clone());
        actor_runtime.set_session_mgr(session_manager.clone());
        actor_runtime.set_ocapn_slots(ocapn_slots.clone());
        actor_runtime.set_connected_peers(connected_peers.clone());
        actor_runtime.set_ocapn_call_results(ocapn_call_results.clone());
        actor_runtime.set_node_mailboxes(node_mailboxes.clone());
        actor_runtime.set_wasm_runner(resources.wasm.clone());
        actor_runtime.set_slot_owners(ocapn_slot_owners.clone());
        actor_runtime.set_egui_ctx(cc.egui_ctx.clone());

        Self {
            graph,
            registry,
            resources,
            actor_runtime,
            canvas_state: CanvasState::new(),
            panel_state: PanelState::new(),
            undo_history,
            current_file,
            theme_applied: false,
            pending_nodes: HashSet::new(),
            debug_log: DebugLog::new(),
            net_handle,
            net_values,
            tick_nodes: HashMap::new(),
            session_manager,
            ocapn_slots,
            connected_peers,
            ocapn_call_results,
            node_mailboxes,
            ocapn_slot_owners,
            actor_nodes: HashSet::new(),
            node_windows: HashMap::new(),
            closed_windows: HashSet::new(),
            explicit_compute: HashSet::new(),
        }
    }

    fn compute_node(&mut self, node_id: NodeId) {
        // Skip phantom nodes — they have no code to execute
        if self.graph.nodes.get(&node_id).map_or(false, |n| n.phantom) { return; }
        if let Some((node, template, inputs)) = self.resolve_node(node_id) {
            self.pending_nodes.insert(node_id);
            self.actor_runtime.compute(node_id, node, template, inputs, self.resources.db.clone());
        }
    }

    fn compute_node_debounced(&mut self, node_id: NodeId) {
        if self.graph.nodes.get(&node_id).map_or(false, |n| n.phantom) { return; }
        if let Some((node, template, inputs)) = self.resolve_node(node_id) {
            self.pending_nodes.insert(node_id);
            self.actor_runtime.compute_debounced(node_id, node, template, inputs, self.resources.db.clone());
        }
    }

    fn resolve_node(&self, node_id: NodeId) -> Option<(Node, Option<NodeTemplate>, HashMap<String, Value>)> {
        let node = self.graph.nodes.get(&node_id)?;
        let template = self.registry.templates.get(&node.template_name).cloned();
        let available_inputs = self.graph.resolve_all_input_values(node_id);
        Some((node.clone(), template, available_inputs))
    }

    fn poll_worker_results(&mut self) {
        while let Some(result) = self.actor_runtime.poll() {
            match result {
                ActorResult::Computed { node_id, result, .. } => {
                    self.pending_nodes.remove(&node_id);
                    let label = self.graph.nodes.get(&node_id)
                        .map(|n| n.label.clone()).unwrap_or_default();
                    self.debug_log.log("compute", format!(
                        "#{} \"{}\" → {} blocks, {} outputs",
                        node_id, label,
                        result.render_blocks.len(),
                        result.output_values.len()
                    ));
                    // For define-module nodes: derive input ports from imported modules' exports
                    let derived_inputs: Vec<PortDef> = if let Some(code) = self.graph.nodes.get(&node_id).map(|n| n.script_code.clone()) {
                        if let Some(header) = crate::scheme_engine::parse_module_header(&code) {
                            let mut inputs = Vec::new();
                            for import_label in &header.imports {
                                if let Some(src_id) = self.graph.find_node_by_import_label(import_label) {
                                    if let Some(src) = self.graph.nodes.get(&src_id) {
                                        for port in &src.script_outputs {
                                            if !inputs.iter().any(|p: &PortDef| p.name == port.name) {
                                                inputs.push(port.clone());
                                            }
                                        }
                                    }
                                }
                            }
                            inputs
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    };

                    if let Some(n) = self.graph.nodes.get_mut(&node_id) {
                        n.render_blocks = result.render_blocks;
                        n.error = None;
                        for (name, val) in &result.output_values {
                            n.output_values.insert(name.clone(), val.clone());
                        }
                        // Update dynamic ports from script result
                        if !result.declared_inputs.is_empty() || !result.declared_outputs.is_empty() {
                            n.script_inputs = result.declared_inputs.iter()
                                .map(|(name, type_str)| PortDef {
                                    name: name.clone(),
                                    port_type: PortType::from_str(type_str).unwrap_or(PortType::F64),
                                })
                                .collect();
                            n.script_outputs = result.declared_outputs.iter()
                                .map(|(name, type_str)| PortDef {
                                    name: name.clone(),
                                    port_type: PortType::from_str(type_str).unwrap_or(PortType::F64),
                                })
                                .collect();
                        }
                        // Override script_inputs for define-module nodes with derived imports
                        if !derived_inputs.is_empty() {
                            n.script_inputs = derived_inputs;
                        }
                        n.widget_decls = result.widget_decls;
                    }
                    // Handle tick requests
                    if let Some(ms) = result.tick_interval_ms {
                        self.tick_nodes.insert(node_id, (ms, Instant::now()));
                    } else {
                        self.tick_nodes.remove(&node_id);
                    }
                    // Track actor nodes (those with on-message handlers)
                    if result.has_message_handler {
                        self.actor_nodes.insert(node_id);
                    } else {
                        self.actor_nodes.remove(&node_id);
                    }
                    // Track node windows — only open if user explicitly computed this node
                    if let Some(title) = result.window_title {
                        if self.explicit_compute.remove(&node_id) {
                            self.closed_windows.remove(&node_id);
                            self.node_windows.insert(node_id, title);
                        }
                    }
                    // Re-register outputs so downstream nodes can read them
                    if let Some(node) = self.graph.nodes.get(&node_id) {
                        // Register by label
                        self.actor_runtime.engine().register_node_library_named(node_id, Some(&node.label), &node.output_values);
                        // Also register by module name if different from label
                        if let Some(header) = crate::scheme_engine::parse_module_header(&node.script_code) {
                            let safe_label = node.label.replace(' ', "-");
                            if header.name != safe_label && !header.name.is_empty() {
                                self.actor_runtime.engine().register_node_library_named(node_id, Some(&header.name), &node.output_values);
                            }
                        }
                    }
                    // Auto-publish: any node with define-module exports → broadcast to network
                    if let Some(node) = self.graph.nodes.get(&node_id) {
                        if !node.phantom {
                            if let Some(header) = crate::scheme_engine::parse_module_header(&node.script_code) {
                                if !header.exports.is_empty() && !header.name.is_empty() {
                                    let channel = header.name.clone();
                                    let mut values = HashMap::new();
                                    for (k, v) in &node.output_values {
                                        values.insert(k.clone(), v.clone());
                                    }
                                    for (k, v) in &node.widget_values {
                                        values.insert(k.clone(), v.clone());
                                    }
                                    // Local loopback
                                    {
                                        let mut store = self.net_values.lock().unwrap();
                                        store.insert(("local".to_string(), channel.clone()), values.clone());
                                    }
                                    // Send to network peers
                                    self.net_handle.send(NetCommand::Publish {
                                        channel: channel.clone(),
                                        values,
                                    });
                                    self.debug_log.log("net", format!(
                                        "auto-publish \"{}\" → {:?}",
                                        channel,
                                        node.output_values.keys().collect::<Vec<_>>()
                                    ));
                                }
                            }
                        }
                    }
                    // Handle OCapN sends
                    for (peer_id, message) in result.ocapn_sends {
                        self.debug_log.log("ocapn", format!(
                            "send to {}...: {:?}",
                            &peer_id[..12.min(peer_id.len())],
                            message
                        ));
                        self.net_handle.send(NetCommand::OCapNSend {
                            peer_id,
                            message,
                        });
                    }
                    // Handle actor recompute requests (from node-send)
                    for target_id in result.recompute_requests {
                        if !self.pending_nodes.contains(&target_id) {
                            self.compute_node(target_id);
                        }
                    }
                    // Reactive dataflow: dispatch direct downstream nodes.
                    let direct_downstream = self.graph.direct_downstream(node_id);
                    for did in direct_downstream {
                        if !self.pending_nodes.contains(&did) {
                            self.compute_node(did);
                        }
                    }
                }
                ActorResult::Error { node_id, message } => {
                    let label = self.graph.nodes.get(&node_id)
                        .map(|n| n.label.clone()).unwrap_or_default();
                    eprintln!("ERROR node #{} \"{}\": {}", node_id, label, &message);
                    self.debug_log.log("error", format!("#{}: {}", node_id, &message));
                    self.pending_nodes.remove(&node_id);
                    if let Some(n) = self.graph.nodes.get_mut(&node_id) {
                        n.error = Some(message);
                    }
                }
            }
        }
    }

    fn poll_network(&mut self) {
        let events = self.net_handle.poll();
        let mut need_recompute = false;
        for event in events {
            match event {
                NetEvent::PeerDiscovered(peer) => {
                    self.debug_log.log("net", format!("peer discovered: {}...", &peer[..12.min(peer.len())]));
                    self.connected_peers.lock().unwrap().insert(peer.clone());
                    self.session_manager.lock().unwrap().ensure_session(&peer);
                }
                NetEvent::PeerLost(peer) => {
                    self.debug_log.log("net", format!("peer lost: {}...", &peer[..12.min(peer.len())]));
                    self.connected_peers.lock().unwrap().remove(&peer);
                    self.session_manager.lock().unwrap().remove_session(&peer);
                    // Remove values from this peer
                    let mut store = self.net_values.lock().unwrap();
                    store.retain(|(p, _), _| *p != peer);
                    drop(store);
                    // Remove phantom nodes from this peer
                    let phantom_ids: Vec<NodeId> = self.graph.nodes.iter()
                        .filter(|(_, n)| n.phantom && n.remote_peer.as_deref() == Some(&peer))
                        .map(|(&id, _)| id)
                        .collect();
                    for id in phantom_ids {
                        self.debug_log.log("net", format!("removing phantom node #{}", id));
                        self.graph.remove_node(id);
                    }
                }
                NetEvent::ValuesReceived { peer, channel, values } => {
                    self.debug_log.log("net", format!(
                        "recv \"{}\": {:?}",
                        channel,
                        values.keys().collect::<Vec<_>>()
                    ));
                    {
                        let mut store = self.net_values.lock().unwrap();
                        store.insert((peer.clone(), channel.clone()), values.clone());
                    }

                    // Phantom node logic: skip if local node with this module name exists
                    let has_local = self.graph.nodes.values().any(|n| {
                        !n.phantom && crate::scheme_engine::parse_module_header(&n.script_code)
                            .map_or(false, |h| h.name == channel)
                    });

                    if !has_local {
                        // Find existing phantom for this channel, or create one
                        let phantom_id = self.graph.nodes.iter()
                            .find(|(_, n)| n.phantom && n.label == channel)
                            .map(|(&id, _)| id);

                        let pid = if let Some(id) = phantom_id {
                            // Update existing phantom
                            if let Some(n) = self.graph.nodes.get_mut(&id) {
                                n.output_values = values.clone();
                                n.remote_peer = Some(peer.clone());
                                n.script_outputs = values.keys()
                                    .map(|k| PortDef { name: k.clone(), port_type: PortType::F64 })
                                    .collect();
                            }
                            id
                        } else {
                            // Create new phantom node
                            let id = self.graph.next_node_id;
                            self.graph.next_node_id += 1;
                            // Position: stack phantoms on the right side
                            let phantom_count = self.graph.nodes.values().filter(|n| n.phantom).count();
                            let node = Node {
                                id,
                                template_name: "Script".to_string(),
                                label: channel.clone(),
                                pos: [900.0, 50.0 + phantom_count as f32 * 200.0],
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
                            };
                            self.graph.nodes.insert(id, node);
                            self.debug_log.log("net", format!(
                                "created phantom node #{} \"{}\" from peer {}...",
                                id, channel, &peer[..12.min(peer.len())]
                            ));
                            id
                        };

                        // Register R6RS library so local nodes can (use-module (node <channel>))
                        self.actor_runtime.engine().register_node_library_named(
                            pid, Some(&channel), &values,
                        );

                        // Recompute downstream nodes that import this module
                        let downstream: Vec<NodeId> = self.graph.nodes.iter()
                            .filter(|(_, n)| !n.phantom && crate::scheme_engine::extract_imports(&n.script_code).contains(&channel))
                            .map(|(id, _)| *id)
                            .collect();
                        for did in downstream {
                            if !self.pending_nodes.contains(&did) {
                                self.compute_node(did);
                            }
                        }
                    }
                    need_recompute = true;
                }
                NetEvent::OCapNReceived { peer, message } => {
                    self.debug_log.log("ocapn", format!(
                        "recv from {}...",
                        &peer[..12.min(peer.len())]
                    ));
                    // For OpDeliver, the network thread already handled delivery and response.
                    // We still trigger recompute for nodes using ocapn-receive.
                    if let crate::ocapn::types::OCapNMessage::OpDeliver { to_desc: _, args, .. } = &message {
                        // Trigger recompute for ocapn-receive nodes
                        if args.len() >= 2 {
                            if let crate::ocapn::syrup::SyrupValue::Symbol(method) = &args[0] {
                                if method == "deliver-to" {
                                    let ocapn_nodes: Vec<NodeId> = self.graph.nodes.iter()
                                        .filter(|(_, n)| n.template_name == "Script" && n.script_code.contains("ocapn-receive"))
                                        .map(|(id, _)| *id).collect();
                                    for nid in ocapn_nodes {
                                        if !self.pending_nodes.contains(&nid) {
                                            self.compute_node(nid);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let crate::ocapn::types::OCapNMessage::OpDeliverOnly { to_desc: _, args } = &message {
                        // Convention: args = [Symbol("deliver-to"), Bytestring(swiss), ...]
                        if args.len() >= 2 {
                            if let (
                                crate::ocapn::syrup::SyrupValue::Symbol(method),
                                swiss_val,
                            ) = (&args[0], &args[1]) {
                                if method == "deliver-to" {
                                    if let Some(swiss) = crate::ocapn::types::SwissNum::from_syrup(swiss_val) {
                                        let swiss_hex = swiss.to_hex();
                                        let remaining_args = &args[2..];
                                        let mgr = self.session_manager.lock().unwrap();
                                        match mgr.deliver_by_swiss(&swiss, remaining_args) {
                                            Ok(result) => {
                                                self.debug_log.log("ocapn", format!(
                                                    "deliver ok: {:?}", result
                                                ));
                                                drop(mgr);

                                                // Route to owner node's mailbox
                                                let owner_id = self.ocapn_slot_owners.lock().unwrap()
                                                    .get(&swiss_hex).copied();
                                                if let Some(owner_id) = owner_id {
                                                    let mut mailboxes = self.node_mailboxes.lock().unwrap();
                                                    mailboxes.entry(owner_id).or_default()
                                                        .push_back(remaining_args.to_vec());
                                                    drop(mailboxes);
                                                    if !self.pending_nodes.contains(&owner_id) {
                                                        self.compute_node(owner_id);
                                                    }
                                                }

                                                // Also trigger recompute on nodes using ocapn-receive
                                                let ocapn_nodes: Vec<NodeId> = self.graph.nodes.iter()
                                                    .filter(|(_, n)| n.template_name == "Script" && n.script_code.contains("ocapn-receive"))
                                                    .map(|(id, _)| *id).collect();
                                                for nid in ocapn_nodes {
                                                    if !self.pending_nodes.contains(&nid) {
                                                        self.compute_node(nid);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                self.debug_log.log("ocapn", format!(
                                                    "deliver error: {}", e
                                                ));
                                            }
                                        }
                                    } else {
                                        self.debug_log.log("ocapn", String::from("invalid swiss-num in deliver-to"));
                                    }
                                } else {
                                    self.debug_log.log("ocapn", format!("unknown method: {}", method));
                                }
                            }
                        }
                    }
                }
                NetEvent::OCapNCallResult { request_id, value } => {
                    self.debug_log.log("ocapn", format!(
                        "call result {}: {:?}", request_id, value
                    ));
                    self.ocapn_call_results.lock().unwrap().insert(request_id, value);
                    // Trigger recompute on nodes using ocapn-call-result
                    let ocapn_nodes: Vec<NodeId> = self.graph.nodes.iter()
                        .filter(|(_, n)| n.template_name == "Script" && n.script_code.contains("ocapn-call-result"))
                        .map(|(id, _)| *id).collect();
                    for nid in ocapn_nodes {
                        if !self.pending_nodes.contains(&nid) {
                            self.compute_node(nid);
                        }
                    }
                }
                NetEvent::LocalPeerId(peer_id) => {
                    self.debug_log.log("net", format!("local peer: {}...", &peer_id[..12.min(peer_id.len())]));
                    self.session_manager.lock().unwrap().set_local_peer_id(peer_id);
                }
            }
        }
        // Recompute nodes that still use legacy net-value (backward compat)
        if need_recompute {
            let script_nodes: Vec<NodeId> = self.graph.nodes.iter()
                .filter(|(_, n)| !n.phantom && n.template_name == "Script" && n.script_code.contains("net-value"))
                .map(|(id, _)| *id)
                .collect();
            for nid in script_nodes {
                if !self.pending_nodes.contains(&nid) {
                    self.compute_node(nid);
                }
            }
        }
    }

    fn poll_ticks(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let mut to_recompute = Vec::new();
        for (node_id, (interval_ms, last_tick)) in &mut self.tick_nodes {
            let elapsed = now.duration_since(*last_tick).as_millis() as u64;
            if elapsed >= *interval_ms {
                *last_tick = now;
                to_recompute.push(*node_id);
            }
        }
        for nid in to_recompute {
            if !self.pending_nodes.contains(&nid) {
                self.compute_node(nid);
            }
        }
        if !self.tick_nodes.is_empty() {
            ctx.request_repaint();
        }
    }

    fn handle_actions(&mut self, actions: Vec<PanelAction>) {
        for action in actions {
            match action {
                PanelAction::ComputeNode(id) => {
                    self.explicit_compute.insert(id);
                    // Compute root ancestors (no upstream imports) — cascade handles the rest.
                    let ancestors = self.graph.ancestors_sorted(id);
                    let roots: Vec<NodeId> = ancestors.iter().copied()
                        .filter(|&aid| {
                            let is_root = self.graph.nodes.get(&aid)
                                .map_or(true, |n| crate::scheme_engine::extract_imports(&n.script_code).is_empty());
                            is_root || aid == id
                        })
                        .collect();
                    for rid in roots {
                        if !self.pending_nodes.contains(&rid) {
                            self.compute_node(rid);
                        }
                    }
                }
                PanelAction::CancelCompute => {
                    self.actor_runtime.cancel_all();
                    self.pending_nodes.clear();
                }
                PanelAction::RecomputeSelected => {
                    if let Some(node_id) = self.panel_state.selected_node {
                        if let Some(n) = self.graph.nodes.get_mut(&node_id) {
                            n.render_blocks.clear();
                        }
                        self.compute_node(node_id);
                    }
                }
                PanelAction::SaveGraph => {
                    self.save_graph();
                }
                PanelAction::LoadGraph => {
                    self.load_graph();
                }
                PanelAction::AddNode(name, pos) => {
                    if let Some(template) = self.registry.templates.get(&name) {
                        let template = template.clone();
                        let _id = self.graph.add_node(&template, pos);
                        self.undo_history.push(&self.graph);
    
                    }
                }
                PanelAction::DeleteNode(id) => {
                    self.graph.remove_node(id);
                    self.actor_runtime.remove(id);
                    self.actor_nodes.remove(&id);
                    self.node_windows.remove(&id);
                    self.closed_windows.remove(&id);
                    self.undo_history.push(&self.graph);

                    if self.panel_state.selected_node == Some(id) {
                        self.panel_state.selected_node = None;
                    }
                }
                PanelAction::SendMessage(node_id, message_parts) => {
                    // Convert message parts to SyrupValues and push to mailbox
                    let msg: Vec<crate::ocapn::syrup::SyrupValue> = message_parts.iter()
                        .map(|s| {
                            if let Ok(n) = s.parse::<f64>() {
                                crate::ocapn::syrup::SyrupValue::Float64(n)
                            } else {
                                crate::ocapn::syrup::SyrupValue::Symbol(s.clone())
                            }
                        })
                        .collect();
                    self.node_mailboxes.lock().unwrap()
                        .entry(node_id).or_default().push_back(msg);
                    if !self.pending_nodes.contains(&node_id) {
                        self.compute_node(node_id);
                    }
                }
                PanelAction::UpdateWidget(node_id, key, val) => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        node.widget_values.insert(key, val);
                    }
                    if let Some(node) = self.graph.nodes.get(&node_id) {
                        self.actor_runtime.engine().register_node_library_named(node_id, Some(&node.label), &node.output_values);
                    }
                    self.compute_node_debounced(node_id);
                }
            }
        }
    }

    fn save_graph(&mut self) {
        let path = self.current_file.clone().or_else(|| {
            rfd::FileDialog::new()
                .set_title("Save Graph")
                .add_filter("Scheme", &["scm"])
                .add_filter("JSON", &["json"])
                .save_file()
        });

        if let Some(path) = path {
            let result = if path.extension().map_or(false, |e| e == "scm") {
                persistence::save_graph_scm(&self.graph, &path, &self.resources.db)
            } else {
                persistence::save_graph(&self.graph, &path, &self.resources.db)
            };
            if let Err(e) = result {
                log::error!("Failed to save graph: {}", e);
            } else {
                self.current_file = Some(path);
            }
        }
    }

    fn load_graph(&mut self) {
        let path = rfd::FileDialog::new()
            .set_title("Load Graph")
            .add_filter("Graph files", &["scm", "json"])
            .pick_file();

        if let Some(path) = path {
            let result = if path.extension().map_or(false, |e| e == "scm") {
                persistence::load_graph_scm(&path, &self.resources.db)
            } else {
                persistence::load_graph(&path, &self.resources.db)
            };
            match result {
                Ok(graph) => {
                    self.graph = graph;
                    self.undo_history.push(&self.graph);
                    self.current_file = Some(path);
                }
                Err(e) => {
                    log::error!("Failed to load graph: {}", e);
                }
            }
        }
    }
}

impl eframe::App for WasmCanvasApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_applied {
            theme::apply_theme(ctx);
            self.theme_applied = true;
        }

        // Poll background worker results, network events, and ticks
        self.poll_worker_results();
        self.poll_network();
        self.poll_ticks(ctx);

        // Keep repainting while work is pending
        if !self.pending_nodes.is_empty() || self.actor_runtime.has_pending() {
            ctx.request_repaint();
        }

        // Keyboard shortcuts
        let mut actions = Vec::new();

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Z)) {
            if let Some(graph) = self.undo_history.undo() {
                self.graph = graph;
            }
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Y)) {
            if let Some(graph) = self.undo_history.redo() {
                self.graph = graph;
            }
        }

        // Toolbar
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            let toolbar_actions = panels::draw_toolbar(ui);
            actions.extend(toolbar_actions);
        });

        // Bottom panel - debug
        if self.panel_state.show_debug {
            egui::TopBottomPanel::bottom("bottom_panel")
                .resizable(true)
                .default_height(150.0)
                .show(ctx, |ui| {
                    panels::draw_debug_panel(ui, &self.resources.db, &mut self.debug_log);
                });
        }

        // Toggle log with backtick, debug with D
        if ctx.input(|i| i.key_pressed(egui::Key::Backtick)) {
            if self.panel_state.show_debug {
                self.panel_state.show_debug = false;
            } else {
                self.panel_state.show_log = !self.panel_state.show_log;
            }
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::D)) {
            self.panel_state.show_debug = !self.panel_state.show_debug;
            if self.panel_state.show_debug {
                self.panel_state.show_log = false;
            }
        }

        // Left panel - node library
        if self.panel_state.show_library {
            egui::SidePanel::left("library")
                .default_width(240.0)
                .resizable(true)
                .show(ctx, |ui| {
                    let lib_actions =
                        panels::draw_library(ui, &self.registry, &mut self.panel_state);
                    actions.extend(lib_actions);
                });
        }

        // Right panel - inspector
        if self.panel_state.show_inspector {
            egui::SidePanel::right("inspector")
                .default_width(280.0)
                .resizable(true)
                .show(ctx, |ui| {
                    let sel_id = self.panel_state.selected_node;
                    let computing = sel_id.map_or(false, |id| self.pending_nodes.contains(&id));
                    let win_title = sel_id.and_then(|id| self.node_windows.get(&id).map(|s| s.as_str()));
                    let insp_actions = panels::draw_inspector(
                        ui,
                        &mut self.graph,
                        &self.registry,
                        &mut self.panel_state,
                        &self.resources.db,
                        computing,
                        &mut self.debug_log,
                        win_title,
                    );
                    actions.extend(insp_actions);
                });
        }

        // Central panel - canvas
        egui::CentralPanel::default().show(ctx, |ui| {
            let canvas_response = canvas::draw_canvas(
                ui,
                &mut self.graph,
                &self.registry,
                &mut self.canvas_state,
            );

            // Handle canvas responses
            if let Some(node_id) = canvas_response.node_selected {
                self.panel_state.selected_node = Some(node_id);
            }

            if let Some((from_node, _from_port, to_node, _to_port)) = canvas_response.new_connection
            {
                // Wire drag → insert (import (node <label>)) into target node's code
                if let Some(source_node) = self.graph.nodes.get(&from_node) {
                    let source_label = source_node.label.replace(' ', "-");
                    let import_line = format!("(import (node {}))", source_label);
                    if let Some(target_node) = self.graph.nodes.get_mut(&to_node) {
                        if !target_node.script_code.contains(&import_line) {
                            target_node.script_code = format!("{}\n{}", import_line, target_node.script_code);
                        }
                    }
                }
                self.undo_history.push(&self.graph);
                self.compute_node(to_node);
            }

            for node_id in canvas_response.delete_nodes {
                self.graph.remove_node(node_id);
            }

            for (node_id, key, val) in canvas_response.widget_updates {
                if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                    node.widget_values.insert(key, val);
                }
                if let Some(node) = self.graph.nodes.get(&node_id) {
                    self.actor_runtime.engine().register_node_library_named(node_id, Some(&node.label), &node.output_values);
                }
                self.compute_node_debounced(node_id);
            }

            // Context menu for adding nodes
            if self.canvas_state.show_context_menu {
                let menu_pos = self.canvas_state.context_menu_pos.unwrap_or_default();
                egui::Area::new(egui::Id::new("canvas_context_menu"))
                    .fixed_pos(menu_pos)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(150.0);
                            ui.label(
                                egui::RichText::new("Add Node")
                                    .color(theme::ACCENT)
                                    .strong(),
                            );
                            ui.separator();

                            let mut close = false;
                            for (category, templates) in self.registry.grouped_templates() {
                                ui.label(
                                    egui::RichText::new(&category)
                                        .color(theme::TEXT_DIM)
                                        .small(),
                                );
                                for template in &templates {
                                    if ui.button(&template.name).clicked() {
                                        // Convert screen pos to graph pos
                                        let graph_x = (menu_pos.x - self.graph.viewport_offset[0])
                                            / self.graph.viewport_zoom;
                                        let graph_y = (menu_pos.y - self.graph.viewport_offset[1])
                                            / self.graph.viewport_zoom;
                                        actions.push(PanelAction::AddNode(
                                            template.name.clone(),
                                            [graph_x, graph_y],
                                        ));
                                        close = true;
                                    }
                                }
                            }

                            if close || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                self.canvas_state.show_context_menu = false;
                            }
                        });
                    });

                // Close on click outside
                if ctx.input(|i| {
                    i.pointer.any_click() && !i.pointer.secondary_clicked()
                }) {
                    self.canvas_state.show_context_menu = false;
                }
            }
        });

        self.handle_actions(actions);

        // Render node windows as separate native viewports
        let window_entries: Vec<(NodeId, String, Vec<crate::render::RenderBlock>, Vec<crate::bridge::WidgetDecl>, HashMap<String, Value>)> =
            self.node_windows.iter()
                .filter_map(|(&node_id, title)| {
                    let node = self.graph.nodes.get(&node_id)?;
                    Some((node_id, title.clone(), node.render_blocks.clone(), node.widget_decls.clone(), node.widget_values.clone()))
                })
                .collect();

        let db = self.resources.db.clone();
        let mut window_actions = Vec::new();
        let mut windows_to_close = Vec::new();
        for (node_id, title, blocks, widget_decls, widget_values) in &window_entries {
            let node_id = *node_id;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("node_window", node_id)),
                egui::ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([400.0, 300.0]),
                |ctx, _class| {
                    if ctx.input(|i| i.viewport().close_requested()) {
                        windows_to_close.push(node_id);
                    }
                    egui::CentralPanel::default().show(ctx, |ui| {
                        // Render widget_decls (slider, checkbox)
                        for wdecl in widget_decls {
                            let current = widget_values.get(&wdecl.name)
                                .and_then(|v| match v {
                                    Value::F64(f) => Some(*f),
                                    _ => None,
                                })
                                .unwrap_or(0.0);
                            match wdecl.widget_type.as_str() {
                                "slider" => {
                                    let min = wdecl.params.first().copied().unwrap_or(0.0);
                                    let max = wdecl.params.get(1).copied().unwrap_or(100.0);
                                    let mut val = current;
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(&wdecl.name).color(theme::TEXT_DIM));
                                        if ui.add(egui::Slider::new(&mut val, min..=max)).changed() {
                                            window_actions.push(PanelAction::UpdateWidget(
                                                node_id, wdecl.name.clone(), Value::F64(val),
                                            ));
                                        }
                                    });
                                }
                                "checkbox" => {
                                    let mut checked = current != 0.0;
                                    if ui.checkbox(&mut checked, egui::RichText::new(&wdecl.name).color(theme::TEXT)).changed() {
                                        window_actions.push(PanelAction::UpdateWidget(
                                            node_id, wdecl.name.clone(),
                                            Value::F64(if checked { 1.0 } else { 0.0 }),
                                        ));
                                    }
                                }
                                _ => {}
                            }
                        }
                        if !widget_decls.is_empty() && !blocks.is_empty() {
                            ui.separator();
                        }
                        // Render blocks (read-only: buttons/text-input still work via db)
                        panels::draw_render_blocks_interactive(ui, blocks, &db, &mut self.debug_log, Some(&self.graph), Some(node_id), &mut window_actions);
                    });
                },
            );
        }
        for id in windows_to_close {
            self.node_windows.remove(&id);
            self.closed_windows.insert(id);
        }
        self.handle_actions(window_actions);

    }

    fn on_exit(&mut self) {
        if let Err(e) = persistence::save_db(&self.resources.db, &PathBuf::from("./db.json")) {
            log::error!("Failed to auto-save DB: {}", e);
        }
    }
}
