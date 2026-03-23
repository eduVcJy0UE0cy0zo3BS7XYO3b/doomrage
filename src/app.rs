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
    /// All loaded canvases: canvas_name -> Graph (single source of truth)
    all_graphs: HashMap<String, Graph>,
    current_canvas: String,
    registry: NodeRegistry,
    resources: AppResources,
    actor_runtime: ActorRuntime,
    canvas_state: CanvasState,
    panel_state: PanelState,
    undo_history: UndoHistory,
    theme_applied: bool,
    pending_nodes: HashSet<NodeId>,
    debug_log: DebugLog,
    net_handle: NetHandle,
    net_values: NetValues,
    tick_nodes: HashMap<NodeId, (u64, Instant)>,
    session_manager: SharedSessionManager,
    ocapn_slots: OCapNSlotStore,
    connected_peers: crate::bridge::ConnectedPeers,
    ocapn_call_results: crate::bridge::OCapNCallResults,
    node_mailboxes: crate::bridge::NodeMailboxes,
    ocapn_slot_owners: crate::bridge::OCapNSlotOwners,
    actor_nodes: HashSet<NodeId>,
    node_windows: HashMap<NodeId, (String, String)>,
    closed_windows: HashSet<NodeId>,
    explicit_compute: HashSet<NodeId>,
    window_cache: HashMap<NodeId, (Vec<crate::render::RenderBlock>, Vec<crate::bridge::WidgetDecl>, HashMap<String, Value>)>,
    canvas_list: Vec<String>,
    favorites: Vec<String>,
    saved_relays: Vec<String>,
}

// --- Graph access helpers ---

impl WasmCanvasApp {
    /// Current canvas graph (immutable).
    fn graph(&self) -> &Graph {
        self.all_graphs.get(&self.current_canvas).expect("current canvas missing from all_graphs")
    }

    /// Current canvas graph (mutable). Use all_graphs.get_mut(&self.current_canvas)
    /// directly when you need simultaneous mutable access to other fields.
    fn graph_mut(&mut self) -> &mut Graph {
        self.all_graphs.get_mut(&self.current_canvas).expect("current canvas missing from all_graphs")
    }

    /// Push current graph to undo history (avoids borrow conflict).
    fn undo_push(&mut self) {
        let g = self.all_graphs.get(&self.current_canvas).unwrap();
        self.undo_history.push(g);
    }

    /// Find a node by ID across all canvases (immutable).
    fn find_node(&self, node_id: NodeId) -> Option<&Node> {
        self.all_graphs.values().find_map(|g| g.nodes.get(&node_id))
    }

    /// Find a node by ID and return (canvas_name, &Node).
    fn find_node_canvas(&self, node_id: NodeId) -> Option<(&str, &Node)> {
        self.all_graphs.iter()
            .find_map(|(cname, g)| g.nodes.get(&node_id).map(|n| (cname.as_str(), n)))
    }

    /// Find a node by ID across all canvases (mutable).
    fn find_node_mut(&mut self, node_id: NodeId) -> Option<&mut Node> {
        self.all_graphs.values_mut().find_map(|g| g.nodes.get_mut(&node_id))
    }
}

// --- Construction ---

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

        // Load canvas list and current canvas
        let mut canvas_list = persistence::list_canvases(&resources.db);
        let current_canvas = if canvas_list.is_empty() {
            "default".to_string()
        } else {
            canvas_list[0].clone()
        };

        // Load current canvas graph: DB first, then fallback to .scm/.json import
        let initial_graph = match persistence::load_canvas_from_db(&current_canvas, &resources.db) {
            Ok(Some(g)) => {
                log::info!("Loaded canvas '{}' from DB", current_canvas);
                g
            }
            _ => {
                let scm_path = PathBuf::from("./demo.scm");
                let json_path = PathBuf::from("./demo.json");
                let g = if scm_path.exists() {
                    persistence::load_graph_scm(&scm_path, &resources.db).unwrap_or_else(|e| {
                        log::warn!("Failed to load demo.scm: {}", e);
                        Graph::new()
                    })
                } else if json_path.exists() {
                    persistence::load_graph(&json_path, &resources.db).unwrap_or_else(|e| {
                        log::warn!("Failed to load demo.json: {}", e);
                        Graph::new()
                    })
                } else {
                    Graph::new()
                };
                let _ = persistence::save_canvas_to_db(&current_canvas, &g, &resources.db);
                g
            }
        };

        canvas_list = persistence::list_canvases(&resources.db);
        if !canvas_list.contains(&current_canvas) {
            canvas_list.push(current_canvas.clone());
        }

        // Load all canvases into memory
        let mut all_graphs: HashMap<String, Graph> = HashMap::new();
        for name in &canvas_list {
            if *name == current_canvas {
                all_graphs.insert(name.clone(), initial_graph.clone());
            } else if let Ok(Some(g)) = persistence::load_canvas_from_db(name, &resources.db) {
                all_graphs.insert(name.clone(), g);
            } else {
                all_graphs.insert(name.clone(), Graph::new());
            }
        }

        // Ensure globally unique node IDs across all canvases
        Self::remap_conflicting_ids(&canvas_list, &mut all_graphs);

        // Initialize ports and libraries for current canvas
        {
            let graph = all_graphs.get_mut(&current_canvas).unwrap();
            for node in graph.nodes.values_mut() {
                if let Some(header) = crate::scheme_engine::parse_module_header(&node.script_code) {
                    node.script_outputs = header.exports.iter()
                        .map(|name| PortDef { name: name.clone(), port_type: PortType::F64 })
                        .collect();
                }
            }
            let input_map: HashMap<NodeId, Vec<PortDef>> = graph.nodes.keys().copied().collect::<Vec<_>>()
                .into_iter()
                .filter_map(|id| {
                    let inputs = graph.derive_inputs_for_node(id);
                    if inputs.is_empty() { None } else { Some((id, inputs)) }
                })
                .collect();
            for (id, inputs) in input_map {
                if let Some(node) = graph.nodes.get_mut(&id) {
                    node.script_inputs = inputs;
                }
            }
            resources.scheme.register_stub_libraries(&graph.nodes);
        }

        let favorites = persistence::list_favorites(&resources.db);
        let saved_relays = resources.db.kv_get("saved_relays")
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
            .unwrap_or_default();
        let mut undo_history = UndoHistory::new(10);
        undo_history.push(all_graphs.get(&current_canvas).unwrap());

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
            all_graphs,
            current_canvas,
            registry,
            resources,
            actor_runtime,
            canvas_state: CanvasState::new(),
            panel_state: PanelState::new(),
            undo_history,
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
            window_cache: HashMap::new(),
            canvas_list,
            favorites,
            saved_relays,
        }
    }

    fn remap_conflicting_ids(canvas_list: &[String], all_graphs: &mut HashMap<String, Graph>) {
        let mut global_max: u64 = all_graphs.values()
            .flat_map(|g| g.nodes.keys()).max().copied().unwrap_or(0);
        let mut used_ids: HashSet<u64> = HashSet::new();
        let mut first = true;
        for name in canvas_list {
            if let Some(g) = all_graphs.get_mut(name) {
                let mut remap: Vec<(u64, u64)> = Vec::new();
                for &id in g.nodes.keys() {
                    if first {
                        used_ids.insert(id);
                    } else if used_ids.contains(&id) {
                        global_max += 1;
                        remap.push((id, global_max));
                        used_ids.insert(global_max);
                    } else {
                        used_ids.insert(id);
                    }
                }
                for (old_id, new_id) in &remap {
                    if let Some(mut node) = g.nodes.remove(old_id) {
                        node.id = *new_id;
                        g.nodes.insert(*new_id, node);
                        log::info!("Remapped node #{} → #{} on canvas '{}'", old_id, new_id, name);
                    }
                }
                g.next_node_id = g.nodes.keys().max().copied().unwrap_or(0) + 1;
            }
            first = false;
        }
    }
}

// --- Graph library initialization ---

impl WasmCanvasApp {
    fn init_graph_libraries(&mut self) {
        let graph = self.graph_mut();
        for node in graph.nodes.values_mut() {
            if let Some(header) = crate::scheme_engine::parse_module_header(&node.script_code) {
                node.script_outputs = header.exports.iter()
                    .map(|name| PortDef { name: name.clone(), port_type: PortType::F64 })
                    .collect();
            }
        }
        // Derive script_inputs from imports
        let graph = self.graph();
        let input_map: HashMap<NodeId, Vec<PortDef>> = graph.nodes.keys().copied().collect::<Vec<_>>()
            .into_iter()
            .filter_map(|id| {
                let inputs = graph.derive_inputs_for_node(id);
                if inputs.is_empty() { None } else { Some((id, inputs)) }
            })
            .collect();
        let graph = self.graph_mut();
        for (id, inputs) in input_map {
            if let Some(node) = graph.nodes.get_mut(&id) {
                node.script_inputs = inputs;
            }
        }
        self.resources.scheme.register_stub_libraries(&self.graph().nodes);
    }

}

// --- Compute ---

impl WasmCanvasApp {
    fn compute_if_ready(&mut self, node_id: NodeId) {
        if !self.pending_nodes.contains(&node_id) {
            self.compute_node(node_id);
        }
    }

    fn compute_node(&mut self, node_id: NodeId) {
        if self.find_node(node_id).map_or(false, |n| n.phantom) { return; }
        self.ensure_imports(node_id);
        if let Some((node, template, inputs)) = self.resolve_node(node_id) {
            self.pending_nodes.insert(node_id);
            self.actor_runtime.compute(node_id, node, template, inputs, self.resources.db.clone());
        }
    }

    fn compute_node_debounced(&mut self, node_id: NodeId) {
        if self.find_node(node_id).map_or(false, |n| n.phantom) { return; }
        if let Some((node, template, inputs)) = self.resolve_node(node_id) {
            self.pending_nodes.insert(node_id);
            self.actor_runtime.compute_debounced(node_id, node, template, inputs, self.resources.db.clone());
        }
    }

    /// Before computing a node, ensure cross-canvas imports are available.
    /// For each unresolved import, search other canvases for a matching module
    /// and deliver its current values via loopback.
    fn ensure_imports(&mut self, node_id: NodeId) {
        let (canvas_name, code) = match self.find_node_canvas(node_id) {
            Some((c, n)) => (c.to_string(), n.script_code.clone()),
            None => return,
        };
        let header = match crate::scheme_engine::parse_module_header(&code) {
            Some(h) => h,
            None => return,
        };
        let graph = match self.all_graphs.get(&canvas_name) {
            Some(g) => g,
            None => return,
        };

        // Collect import labels that have no matching node on this canvas
        let mut missing: Vec<String> = Vec::new();
        for label in &header.imports {
            if graph.find_node_by_import_label(label).is_none() {
                missing.push(label.clone());
            }
        }
        for (_ns, module_name) in &header.remote_imports {
            if graph.find_node_by_import_label(module_name).is_none() {
                missing.push(module_name.clone());
            }
        }
        if missing.is_empty() { return; }

        // Search other canvases for modules matching missing imports
        for module_name in &missing {
            for (other_canvas, other_graph) in &self.all_graphs {
                if *other_canvas == canvas_name { continue; }
                if let Some(source_node) = other_graph.nodes.values().find(|n| {
                    !n.phantom && crate::scheme_engine::parse_module_header(&n.script_code)
                        .map_or(false, |h| h.name == *module_name)
                }) {
                    let mut values = source_node.output_values.clone();
                    for (k, v) in &source_node.widget_values {
                        values.insert(k.clone(), v.clone());
                    }
                    if values.is_empty() {
                        if let Some(h) = crate::scheme_engine::parse_module_header(&source_node.script_code) {
                            for exp in &h.exports {
                                values.insert(exp.clone(), Value::F64(0.0));
                            }
                        }
                    }
                    let peer = other_canvas.clone();
                    let channel = module_name.clone();
                    self.deliver_values(&canvas_name, &peer, &channel, &values);
                    break;
                }
            }
        }
    }

    fn resolve_node(&self, node_id: NodeId) -> Option<(Node, Option<NodeTemplate>, HashMap<String, Value>)> {
        // Find the graph containing this node
        let graph = self.all_graphs.values()
            .find(|g| g.nodes.contains_key(&node_id))?;
        let node = graph.nodes.get(&node_id)?;
        let template = self.registry.templates.get(&node.template_name).cloned();
        let available_inputs = graph.resolve_all_input_values(node_id);
        Some((node.clone(), template, available_inputs))
    }
}

// --- Poll worker results ---

impl WasmCanvasApp {
    fn poll_worker_results(&mut self) {
        while let Some(result) = self.actor_runtime.poll() {
            match result {
                ActorResult::Computed { node_id, result, .. } => {
                    self.pending_nodes.remove(&node_id);
                    let label = self.find_node(node_id).map(|n| n.label.clone()).unwrap_or_default();
                    self.debug_log.log("compute", format!(
                        "#{} \"{}\" → {} blocks, {} outputs",
                        node_id, label, result.render_blocks.len(), result.output_values.len()
                    ));

                    self.apply_compute_result(node_id, &result);
                    self.handle_compute_side_effects(node_id, &result);
                    self.register_node_libraries(node_id);
                    self.auto_publish_node(node_id);

                    for (peer_id, message) in result.ocapn_sends {
                        self.debug_log.log("ocapn", format!(
                            "send to {}...: {:?}", &peer_id[..12.min(peer_id.len())], message
                        ));
                        self.net_handle.send(NetCommand::OCapNSend { peer_id, message });
                    }
                    for target_id in result.recompute_requests {
                        self.compute_if_ready(target_id);
                    }

                    self.propagate_downstream(node_id);
                }
                ActorResult::Error { node_id, message } => {
                    let label = self.find_node(node_id).map(|n| n.label.clone()).unwrap_or_default();
                    eprintln!("ERROR node #{} \"{}\": {}", node_id, label, &message);
                    self.debug_log.log("error", format!("#{}: {}", node_id, &message));
                    self.pending_nodes.remove(&node_id);
                    if let Some(n) = self.find_node_mut(node_id) {
                        n.error = Some(message);
                    }
                }
            }
        }
    }

    fn apply_compute_result(&mut self, node_id: NodeId, result: &crate::scheme_engine::ScriptResult) {
        // Derive input ports from imported modules' exports (find node's own graph)
        let derived_inputs: Vec<PortDef> = self.all_graphs.values()
            .find(|g| g.nodes.contains_key(&node_id))
            .map(|g| g.derive_inputs_for_node(node_id))
            .unwrap_or_default();

        if let Some(n) = self.find_node_mut(node_id) {
            n.render_blocks = result.render_blocks.clone();
            n.error = None;
            for (name, val) in &result.output_values {
                n.output_values.insert(name.clone(), val.clone());
            }
            if !result.declared_inputs.is_empty() || !result.declared_outputs.is_empty() {
                n.script_inputs = result.declared_inputs.iter()
                    .map(|(name, type_str)| PortDef {
                        name: name.clone(),
                        port_type: PortType::from_str(type_str).unwrap_or(PortType::F64),
                    }).collect();
                n.script_outputs = result.declared_outputs.iter()
                    .map(|(name, type_str)| PortDef {
                        name: name.clone(),
                        port_type: PortType::from_str(type_str).unwrap_or(PortType::F64),
                    }).collect();
            }
            if !derived_inputs.is_empty() {
                n.script_inputs = derived_inputs;
            }
            n.widget_decls = result.widget_decls.clone();
        }
    }

    fn handle_compute_side_effects(&mut self, node_id: NodeId, result: &crate::scheme_engine::ScriptResult) {
        if let Some(ms) = result.tick_interval_ms {
            self.tick_nodes.insert(node_id, (ms, Instant::now()));
        } else {
            self.tick_nodes.remove(&node_id);
        }
        if result.has_message_handler {
            self.actor_nodes.insert(node_id);
        } else {
            self.actor_nodes.remove(&node_id);
        }
        if let Some(ref title) = result.window_title {
            if self.explicit_compute.remove(&node_id) {
                self.closed_windows.remove(&node_id);
                self.node_windows.insert(node_id, (title.clone(), self.current_canvas.clone()));
            }
        }
    }

    fn register_node_libraries(&self, node_id: NodeId) {
        if let Some(node) = self.find_node(node_id) {
            self.actor_runtime.engine().register_node_library_named(
                node_id, Some(&node.label), &node.output_values,
            );
            if let Some(header) = crate::scheme_engine::parse_module_header(&node.script_code) {
                let safe_label = node.label.replace(' ', "-");
                if header.name != safe_label && !header.name.is_empty() {
                    self.actor_runtime.engine().register_node_library_named(
                        node_id, Some(&header.name), &node.output_values,
                    );
                }
            }
        }
    }

    fn auto_publish_node(&mut self, node_id: NodeId) {
        let info: Option<(String, String, HashMap<String, Value>)> = self.find_node_canvas(node_id)
            .and_then(|(canvas_name, node)| {
                if node.phantom { return None; }
                let header = crate::scheme_engine::parse_module_header(&node.script_code)?;
                if header.exports.is_empty() || header.name.is_empty() { return None; }
                let mut values = node.output_values.clone();
                for (k, v) in &node.widget_values { values.insert(k.clone(), v.clone()); }
                Some((canvas_name.to_string(), header.name.clone(), values))
            });
        if let Some((source_canvas, channel, values)) = info {
            {
                let mut store = self.net_values.lock().unwrap();
                store.insert(("local".to_string(), channel.clone()), values.clone());
            }
            self.net_handle.send(NetCommand::Publish { channel: channel.clone(), values: values.clone() });
            self.debug_log.log("net", format!("auto-publish \"{}\"", channel));

            // Loopback: deliver to all OTHER canvases
            let other_canvases: Vec<String> = self.all_graphs.keys()
                .filter(|k| **k != source_canvas)
                .cloned().collect();
            for other_canvas in other_canvases {
                self.deliver_values(&other_canvas, &source_canvas, &channel, &values);
            }
        }
    }

    fn propagate_downstream(&mut self, node_id: NodeId) {
        // Find which graph this node belongs to
        let canvas_name = self.all_graphs.iter()
            .find(|(_, g)| g.nodes.contains_key(&node_id))
            .map(|(name, _)| name.clone());
        let canvas_name = match canvas_name {
            Some(n) => n,
            None => return,
        };
        let direct_downstream = self.all_graphs.get(&canvas_name).unwrap().direct_downstream(node_id);
        for did in direct_downstream {
            if !self.pending_nodes.contains(&did) {
                // Compute on the node's own graph
                if let Some(n) = self.all_graphs.get(&canvas_name).unwrap().nodes.get(&did) {
                    let template = self.registry.templates.get(&n.template_name).cloned();
                    let inputs = self.all_graphs.get(&canvas_name).unwrap().resolve_all_input_values(did);
                    self.pending_nodes.insert(did);
                    self.actor_runtime.compute(did, n.clone(), template, inputs, self.resources.db.clone());
                }
            }
        }
    }

    /// Deliver values to a specific canvas as if from a peer.
    /// Creates/updates phantom node, registers R6RS libraries, recomputes downstream.
    fn deliver_values(&mut self, canvas_key: &str, peer: &str, channel: &str, values: &HashMap<String, Value>) {
        let graph = match self.all_graphs.get(canvas_key) {
            Some(g) => g,
            None => return,
        };

        // Skip if this canvas has a local node with this module name
        let has_local = graph.nodes.values().any(|n| {
            !n.phantom && crate::scheme_engine::parse_module_header(&n.script_code)
                .map_or(false, |h| h.name == channel)
        });
        if has_local { return; }

        let phantom_id = graph.nodes.iter()
            .find(|(_, n)| n.phantom && n.label == channel)
            .map(|(&id, _)| id);

        let graph = self.all_graphs.get_mut(canvas_key).unwrap();
        let pid = if let Some(id) = phantom_id {
            if let Some(n) = graph.nodes.get_mut(&id) {
                n.output_values = values.clone();
                n.remote_peer = Some(peer.to_string());
                n.script_outputs = values.keys()
                    .map(|k| PortDef { name: k.clone(), port_type: PortType::F64 })
                    .collect();
            }
            id
        } else {
            let id = graph.next_node_id;
            graph.next_node_id += 1;
            let phantom_count = graph.nodes.values().filter(|n| n.phantom).count();
            let node = Node {
                id,
                template_name: "Script".to_string(),
                label: channel.to_string(),
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
                remote_peer: Some(peer.to_string()),
            };
            graph.nodes.insert(id, node);
            self.debug_log.log("net", format!(
                "created phantom node #{} \"{}\" from peer \"{}\" on canvas \"{}\"",
                id, channel, peer, canvas_key
            ));
            id
        };

        // Register R6RS library (node channel) for imports
        self.actor_runtime.engine().register_node_library_named(pid, Some(channel), values);
        // Register R6RS library (peer channel) for explicit addressing
        self.resources.scheme.register_named_library(peer, channel, values);

        // Recompute downstream nodes that import this channel (local or remote)
        let channel_str = channel.to_string();
        let graph = self.all_graphs.get(canvas_key).unwrap();
        let downstream: Vec<NodeId> = graph.nodes.iter()
            .filter(|(_, n)| {
                if n.phantom { return false; }
                // Local import: (use-module (node channel))
                if crate::scheme_engine::extract_imports(&n.script_code).contains(&channel_str) {
                    return true;
                }
                // Remote import: (use-module (peer channel))
                if let Some(header) = crate::scheme_engine::parse_module_header(&n.script_code) {
                    if header.remote_imports.iter().any(|(_, m)| *m == channel_str) {
                        return true;
                    }
                }
                false
            })
            .map(|(id, _)| *id).collect();
        for did in downstream {
            if !self.pending_nodes.contains(&did) {
                if let Some(n) = self.all_graphs.get(canvas_key).unwrap().nodes.get(&did) {
                    let template = self.registry.templates.get(&n.template_name).cloned();
                    let inputs = self.all_graphs.get(canvas_key).unwrap().resolve_all_input_values(did);
                    self.pending_nodes.insert(did);
                    self.actor_runtime.compute(did, n.clone(), template, inputs, self.resources.db.clone());
                }
            }
        }
    }
}

// --- Poll network & ticks ---

impl WasmCanvasApp {
    fn recompute_by_marker(&mut self, marker: &str) {
        let nodes: Vec<NodeId> = self.graph().nodes.iter()
            .filter(|(_, n)| !n.phantom && n.template_name == "Script" && n.script_code.contains(marker))
            .map(|(id, _)| *id).collect();
        for nid in nodes {
            self.compute_if_ready(nid);
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
                    let mut store = self.net_values.lock().unwrap();
                    store.retain(|(p, _), _| *p != peer);
                    drop(store);
                    // Remove phantom nodes from this peer
                    let graph = self.all_graphs.get_mut(&self.current_canvas).unwrap();
                    let phantom_ids: Vec<NodeId> = graph.nodes.iter()
                        .filter(|(_, n)| n.phantom && n.remote_peer.as_deref() == Some(&peer))
                        .map(|(&id, _)| id).collect();
                    for id in &phantom_ids {
                        graph.remove_node(*id);
                    }
                    for id in phantom_ids {
                        self.debug_log.log("net", format!("removing phantom node #{}", id));
                    }
                }
                NetEvent::ValuesReceived { peer, channel, values } => {
                    self.debug_log.log("net", format!(
                        "recv \"{}\": {:?}", channel, values.keys().collect::<Vec<_>>()
                    ));
                    {
                        let mut store = self.net_values.lock().unwrap();
                        store.insert((peer.clone(), channel.clone()), values.clone());
                    }

                    // Deliver to all canvases via unified deliver_values
                    let all_canvas_keys: Vec<String> = self.all_graphs.keys().cloned().collect();
                    for canvas_key in all_canvas_keys {
                        self.deliver_values(&canvas_key, &peer, &channel, &values);
                    }
                    need_recompute = true;
                }
                NetEvent::OCapNReceived { peer, message } => {
                    self.debug_log.log("ocapn", format!("recv from {}...", &peer[..12.min(peer.len())]));
                    if let crate::ocapn::types::OCapNMessage::OpDeliver { to_desc: _, args, .. } = &message {
                        if args.len() >= 2 {
                            if let crate::ocapn::syrup::SyrupValue::Symbol(method) = &args[0] {
                                if method == "deliver-to" {
                                    self.recompute_by_marker("ocapn-receive");
                                }
                            }
                        }
                    }
                    if let crate::ocapn::types::OCapNMessage::OpDeliverOnly { to_desc: _, args } = &message {
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
                                                self.debug_log.log("ocapn", format!("deliver ok: {:?}", result));
                                                drop(mgr);
                                                let owner_id = self.ocapn_slot_owners.lock().unwrap()
                                                    .get(&swiss_hex).copied();
                                                if let Some(owner_id) = owner_id {
                                                    let mut mailboxes = self.node_mailboxes.lock().unwrap();
                                                    mailboxes.entry(owner_id).or_default()
                                                        .push_back(remaining_args.to_vec());
                                                    drop(mailboxes);
                                                    self.compute_if_ready(owner_id);
                                                }
                                                self.recompute_by_marker("ocapn-receive");
                                            }
                                            Err(e) => {
                                                self.debug_log.log("ocapn", format!("deliver error: {}", e));
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
                    self.debug_log.log("ocapn", format!("call result {}: {:?}", request_id, value));
                    self.ocapn_call_results.lock().unwrap().insert(request_id, value);
                    self.recompute_by_marker("ocapn-call-result");
                }
                NetEvent::LocalPeerId(peer_id) => {
                    self.debug_log.log("net", format!("local peer: {}...", &peer_id[..12.min(peer_id.len())]));
                    self.session_manager.lock().unwrap().set_local_peer_id(peer_id);
                }
            }
        }
        if need_recompute {
            self.recompute_by_marker("net-value");
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
            self.compute_if_ready(nid);
        }
        if !self.tick_nodes.is_empty() {
            ctx.request_repaint();
        }
    }
}

// --- Action handling ---

impl WasmCanvasApp {
    fn handle_actions(&mut self, actions: Vec<PanelAction>) {
        for action in actions {
            match action {
                PanelAction::ComputeNode(id) => {
                    self.explicit_compute.insert(id);
                    let graph = self.graph();
                    let ancestors = graph.ancestors_sorted(id);
                    let roots: Vec<NodeId> = ancestors.iter().copied()
                        .filter(|&aid| {
                            let is_root = graph.nodes.get(&aid)
                                .map_or(true, |n| crate::scheme_engine::extract_imports(&n.script_code).is_empty());
                            is_root || aid == id
                        }).collect();
                    for rid in roots {
                        self.compute_if_ready(rid);
                    }
                }
                PanelAction::CancelCompute => {
                    self.actor_runtime.cancel_all();
                    self.pending_nodes.clear();
                }
                PanelAction::RecomputeSelected => {
                    if let Some(node_id) = self.panel_state.selected_node {
                        if let Some(n) = self.graph_mut().nodes.get_mut(&node_id) {
                            n.render_blocks.clear();
                        }
                        self.compute_node(node_id);
                    }
                }
                PanelAction::SaveGraph => {
                    self.save_graph();
                }
                PanelAction::LoadGraph => {} // legacy, handled by ImportScm
                PanelAction::AddNode(name, pos) => {
                    if let Some(template) = self.registry.templates.get(&name) {
                        let template = template.clone();
                        let _id = self.graph_mut().add_node(&template, pos);
                        self.undo_push();
                    }
                }
                PanelAction::DeleteNode(id) => {
                    self.graph_mut().remove_node(id);
                    self.actor_runtime.remove(id);
                    self.actor_nodes.remove(&id);
                    self.node_windows.remove(&id);
                    self.closed_windows.remove(&id);
                    self.undo_push();
                    if self.panel_state.selected_node == Some(id) {
                        self.panel_state.selected_node = None;
                    }
                }
                PanelAction::SendMessage(node_id, message_parts) => {
                    let msg: Vec<crate::ocapn::syrup::SyrupValue> = message_parts.iter()
                        .map(|s| {
                            if let Ok(n) = s.parse::<f64>() {
                                crate::ocapn::syrup::SyrupValue::Float64(n)
                            } else {
                                crate::ocapn::syrup::SyrupValue::Symbol(s.clone())
                            }
                        }).collect();
                    self.node_mailboxes.lock().unwrap()
                        .entry(node_id).or_default().push_back(msg);
                    self.compute_if_ready(node_id);
                }
                PanelAction::UpdateWidget(node_id, key, val) => {
                    self.handle_update_widget(node_id, key, val);
                }
                PanelAction::AddToFavorites(node_id) => {
                    if let Some(node) = self.graph().nodes.get(&node_id) {
                        let _ = persistence::save_favorite(
                            &node.label, &node.script_code, &node.widget_values, &self.resources.db
                        );
                        self.favorites = persistence::list_favorites(&self.resources.db);
                    }
                }
                PanelAction::InsertFavorite(label) => {
                    if let Some((script_code, widget_values)) = persistence::load_favorite(&label, &self.resources.db) {
                        if let Some(template) = self.registry.templates.get("Script") {
                            let template = template.clone();
                            let id = self.graph_mut().add_node(&template, [200.0, 200.0]);
                            if let Some(node) = self.graph_mut().nodes.get_mut(&id) {
                                node.label = label;
                                node.script_code = script_code;
                                node.widget_values = widget_values;
                            }
                            self.undo_push();
                        }
                    }
                }
                PanelAction::RemoveFavorite(label) => {
                    let _ = persistence::remove_favorite(&label, &self.resources.db);
                    self.favorites = persistence::list_favorites(&self.resources.db);
                }
                PanelAction::NewCanvas(name) => {
                    let global_max = self.all_graphs.values()
                        .flat_map(|g| g.nodes.keys()).max().copied().unwrap_or(0);
                    let mut new_graph = Graph::new();
                    new_graph.next_node_id = global_max + 1;
                    self.all_graphs.insert(name.clone(), new_graph);
                    self.current_canvas = name.clone();
                    let _ = persistence::save_canvas_to_db(&self.current_canvas, self.graph(), &self.resources.db);
                    self.canvas_list = persistence::list_canvases(&self.resources.db);
                    self.panel_state.selected_node = None;
                }
                PanelAction::SwitchCanvas(name) => {
                    self.current_canvas = name;
                    self.init_graph_libraries();
                    self.panel_state.selected_node = None;
                }
                PanelAction::DeleteCanvas(name) => {
                    if self.canvas_list.len() > 1 {
                        let _ = persistence::delete_canvas(&name, &self.resources.db);
                        self.all_graphs.remove(&name);
                        self.canvas_list = persistence::list_canvases(&self.resources.db);
                        if self.current_canvas == name {
                            let next = self.canvas_list.first().cloned().unwrap_or_else(|| "default".to_string());
                            self.current_canvas = next;
                            self.init_graph_libraries();
                            self.panel_state.selected_node = None;
                        }
                        let _ = persistence::save_db(&self.resources.db, &PathBuf::from("./db.json"));
                    }
                }
                PanelAction::ExportScm => {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Export .scm")
                        .add_filter("Scheme", &["scm"])
                        .save_file()
                    {
                        if let Err(e) = persistence::save_graph_scm(self.graph(), &path, &self.resources.db) {
                            log::error!("Failed to export: {}", e);
                        }
                    }
                }
                PanelAction::ImportScm => {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Import .scm")
                        .add_filter("Graph files", &["scm", "json"])
                        .pick_file()
                    {
                        let result = if path.extension().map_or(false, |e| e == "scm") {
                            persistence::load_graph_scm(&path, &self.resources.db)
                        } else {
                            persistence::load_graph(&path, &self.resources.db)
                        };
                        match result {
                            Ok(g) => {
                                self.all_graphs.insert(self.current_canvas.clone(), g);
                                let _ = persistence::save_canvas_to_db(&self.current_canvas, self.graph(), &self.resources.db);
                                self.init_graph_libraries();
                                self.undo_history = UndoHistory::new(10);
                                self.undo_push();
                            }
                            Err(e) => log::error!("Failed to import: {}", e),
                        }
                    }
                }
                PanelAction::SaveRelay(addr) => {
                    if !self.saved_relays.contains(&addr) {
                        self.saved_relays.push(addr);
                        let json = serde_json::to_value(&self.saved_relays).unwrap_or_default();
                        self.resources.db.kv_set("saved_relays", json);
                    }
                }
                PanelAction::ConnectRelay(addr) => {
                    if addr.is_empty() {
                        // Disconnect: show input field again
                        self.panel_state.relay_connected = false;
                    } else {
                        self.net_handle.send(NetCommand::ConnectRelay { addr });
                        self.panel_state.relay_connected = true;
                    }
                }
            }
        }
    }

    fn handle_update_widget(&mut self, node_id: NodeId, key: String, val: Value) {
        // Try current graph first
        if self.graph().nodes.contains_key(&node_id) {
            self.graph_mut().nodes.get_mut(&node_id).unwrap()
                .widget_values.insert(key, val);
            let node = self.graph().nodes.get(&node_id).unwrap();
            self.actor_runtime.engine().register_node_library_named(
                node_id, Some(&node.label), &node.output_values,
            );
            self.compute_node_debounced(node_id);
            return;
        }
        // Node is on another canvas
        let mut to_compute: Option<(Node, Option<NodeTemplate>, HashMap<String, Value>)> = None;
        for (_, other_graph) in &mut self.all_graphs {
            if let Some(node) = other_graph.nodes.get_mut(&node_id) {
                node.widget_values.insert(key, val);
                let template = self.registry.templates.get(&node.template_name).cloned();
                let node_clone = node.clone();
                drop(node);
                let inputs = other_graph.resolve_all_input_values(node_id);
                to_compute = Some((node_clone, template, inputs));
                break;
            }
        }
        if let Some((node_clone, template, inputs)) = to_compute {
            self.actor_runtime.engine().register_node_library_named(
                node_id, Some(&node_clone.label), &node_clone.output_values,
            );
            self.pending_nodes.insert(node_id);
            self.actor_runtime.compute_debounced(node_id, node_clone, template, inputs, self.resources.db.clone());
        }
    }

    fn save_graph(&self) {
        for (name, g) in &self.all_graphs {
            if let Err(e) = persistence::save_canvas_to_db(name, g, &self.resources.db) {
                log::error!("Failed to save canvas '{}' to DB: {}", name, e);
            }
        }
        let _ = persistence::save_db(&self.resources.db, &PathBuf::from("./db.json"));
    }
}

// --- eframe::App ---

impl eframe::App for WasmCanvasApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_applied {
            theme::apply_theme(ctx);
            self.theme_applied = true;
        }

        self.poll_worker_results();
        self.poll_network();
        self.poll_ticks(ctx);

        if !self.pending_nodes.is_empty() || self.actor_runtime.has_pending() {
            ctx.request_repaint();
        }

        let mut actions = Vec::new();

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Z)) {
            if let Some(graph) = self.undo_history.undo() {
                self.all_graphs.insert(self.current_canvas.clone(), graph);
            }
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Y)) {
            if let Some(graph) = self.undo_history.redo() {
                self.all_graphs.insert(self.current_canvas.clone(), graph);
            }
        }

        // Toolbar
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            let toolbar_actions = panels::draw_toolbar(
                ui, &self.current_canvas, &self.canvas_list,
                &mut self.panel_state.new_canvas_name,
                &mut self.panel_state.relay_addr,
                self.panel_state.relay_connected,
                &self.saved_relays,
            );
            actions.extend(toolbar_actions);
        });

        if self.panel_state.show_debug {
            egui::TopBottomPanel::bottom("bottom_panel")
                .resizable(true)
                .default_height(150.0)
                .show(ctx, |ui| {
                    panels::draw_debug_panel(ui, &self.resources.db, &mut self.debug_log);
                });
        }

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

        if self.panel_state.show_library {
            egui::SidePanel::left("library")
                .default_width(240.0)
                .resizable(true)
                .show(ctx, |ui| {
                    let lib_actions = panels::draw_library(
                        ui, &self.registry, &mut self.panel_state, &self.favorites,
                    );
                    actions.extend(lib_actions);
                });
        }

        if self.panel_state.show_inspector {
            egui::SidePanel::right("inspector")
                .default_width(280.0)
                .resizable(true)
                .show(ctx, |ui| {
                    let sel_id = self.panel_state.selected_node;
                    let computing = sel_id.map_or(false, |id| self.pending_nodes.contains(&id));
                    let win_title = sel_id.and_then(|id| self.node_windows.get(&id).map(|(s, _)| s.as_str()));
                    let graph = self.all_graphs.get_mut(&self.current_canvas).unwrap();
                    let insp_actions = panels::draw_inspector(
                        ui, graph, &self.registry, &mut self.panel_state,
                        &self.resources.db, computing, &mut self.debug_log, win_title,
                    );
                    actions.extend(insp_actions);
                });
        }

        // Central panel - canvas
        egui::CentralPanel::default().show(ctx, |ui| {
            let graph = self.all_graphs.get_mut(&self.current_canvas).unwrap();
            let canvas_response = canvas::draw_canvas(
                ui, graph, &self.registry, &mut self.canvas_state,
            );

            if let Some(node_id) = canvas_response.node_selected {
                self.panel_state.selected_node = Some(node_id);
            }

            if let Some((from_node, _from_port, to_node, _to_port)) = canvas_response.new_connection {
                let graph = self.all_graphs.get_mut(&self.current_canvas).unwrap();
                if let Some(source_label) = graph.nodes.get(&from_node).map(|n| n.label.replace(' ', "-")) {
                    let import_line = format!("(import (node {}))", source_label);
                    if let Some(target_node) = graph.nodes.get_mut(&to_node) {
                        if !target_node.script_code.contains(&import_line) {
                            target_node.script_code = format!("{}\n{}", import_line, target_node.script_code);
                        }
                    }
                }
                self.undo_push();
                self.compute_node(to_node);
            }

            for node_id in canvas_response.delete_nodes {
                self.graph_mut().remove_node(node_id);
            }

            for (node_id, key, val) in canvas_response.widget_updates {
                let graph = self.all_graphs.get_mut(&self.current_canvas).unwrap();
                graph.nodes.get_mut(&node_id).map(|n| n.widget_values.insert(key, val));
                if let Some(node) = graph.nodes.get(&node_id) {
                    self.actor_runtime.engine().register_node_library_named(
                        node_id, Some(&node.label), &node.output_values,
                    );
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
                            ui.label(egui::RichText::new("Add Node").color(theme::ACCENT).strong());
                            ui.separator();
                            let mut close = false;
                            let graph = self.all_graphs.get(&self.current_canvas).unwrap();
                            for (category, templates) in self.registry.grouped_templates() {
                                ui.label(egui::RichText::new(&category).color(theme::TEXT_DIM).small());
                                for template in &templates {
                                    if ui.button(&template.name).clicked() {
                                        let graph_x = (menu_pos.x - graph.viewport_offset[0]) / graph.viewport_zoom;
                                        let graph_y = (menu_pos.y - graph.viewport_offset[1]) / graph.viewport_zoom;
                                        actions.push(PanelAction::AddNode(
                                            template.name.clone(), [graph_x, graph_y],
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

                if ctx.input(|i| i.pointer.any_click() && !i.pointer.secondary_clicked()) {
                    self.canvas_state.show_context_menu = false;
                }
            }
        });

        self.handle_actions(actions);

        // Update window cache from each window's home canvas graph
        for (&node_id, (_, canvas_name)) in &self.node_windows {
            if let Some(g) = self.all_graphs.get(canvas_name) {
                if let Some(node) = g.nodes.get(&node_id) {
                    self.window_cache.insert(node_id, (
                        node.render_blocks.clone(),
                        node.widget_decls.clone(),
                        node.widget_values.clone(),
                    ));
                }
            }
        }

        let window_entries: Vec<(NodeId, String, String, Vec<crate::render::RenderBlock>, Vec<crate::bridge::WidgetDecl>, HashMap<String, Value>)> =
            self.node_windows.iter()
                .filter_map(|(&node_id, (title, canvas_name))| {
                    let cached = self.window_cache.get(&node_id)?;
                    Some((node_id, title.clone(), canvas_name.clone(), cached.0.clone(), cached.1.clone(), cached.2.clone()))
                }).collect();

        let db = self.resources.db.clone();
        let mut window_actions = Vec::new();
        let mut windows_to_close = Vec::new();
        for (node_id, title, canvas_name, blocks, widget_decls, widget_values) in &window_entries {
            let node_id = *node_id;
            let fallback_canvas = &self.current_canvas;
            let home_graph = self.all_graphs.get(canvas_name.as_str())
                .or_else(|| self.all_graphs.get(fallback_canvas))
                .expect("no graph available");
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
                        for wdecl in widget_decls {
                            let current = widget_values.get(&wdecl.name)
                                .and_then(|v| match v { Value::F64(f) => Some(*f), _ => None })
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
                        panels::draw_render_blocks_interactive(
                            ui, blocks, &db, &mut self.debug_log,
                            Some(home_graph), Some(node_id), &mut window_actions,
                        );
                    });
                },
            );
        }
        for id in windows_to_close {
            self.node_windows.remove(&id);
            self.window_cache.remove(&id);
            self.closed_windows.insert(id);
        }
        self.handle_actions(window_actions);
    }

    fn on_exit(&mut self) {
        for (name, g) in &self.all_graphs {
            if let Err(e) = persistence::save_canvas_to_db(name, g, &self.resources.db) {
                log::error!("Failed to auto-save canvas '{}': {}", name, e);
            }
        }
        if let Err(e) = persistence::save_db(&self.resources.db, &PathBuf::from("./db.json")) {
            log::error!("Failed to auto-save DB: {}", e);
        }
    }
}
