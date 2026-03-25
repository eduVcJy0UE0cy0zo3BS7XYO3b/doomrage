use crate::actor::ActorRuntime;
use crate::bridge::NetValues;
use crate::db::Db;
use crate::network::{NetCommand, NetHandle};
use crate::nrepl_commands::{NreplCommand, CommandReceiver};
use crate::persistence;
use crate::registry::NodeRegistry;
use crate::scheme_engine::{SchemeEngine, ScriptResult};
use crate::types::*;
use std::collections::{HashMap, HashSet};

/// Shared graph runtime logic that can be used by both the GUI app and headless peer.
/// Owns the graph state, actor runtime, network handle, and registry.
pub struct GraphRuntime {
    pub all_graphs: HashMap<String, Graph>,
    pub actor_runtime: ActorRuntime,
    pub pending_nodes: HashSet<NodeId>,
    pub net_handle: NetHandle,
    pub net_values: NetValues,
    pub user_name: String,
    pub registry: NodeRegistry,
    pub db: Db,
    pub peer_names: HashMap<String, String>,
}

// --- Graph access helpers ---

impl GraphRuntime {
    /// Get a specific canvas graph (immutable).
    pub fn graph(&self, name: &str) -> Option<&Graph> {
        self.all_graphs.get(name)
    }

    /// Get a specific canvas graph (mutable).
    pub fn graph_mut(&mut self, name: &str) -> Option<&mut Graph> {
        self.all_graphs.get_mut(name)
    }

    /// Find a node by ID across all canvases (immutable).
    pub fn find_node(&self, node_id: NodeId) -> Option<&Node> {
        self.all_graphs.values().find_map(|g| g.nodes.get(&node_id))
    }

    /// Find a node by ID and return (canvas_name, &Node).
    pub fn find_node_canvas(&self, node_id: NodeId) -> Option<(&str, &Node)> {
        self.all_graphs.iter()
            .find_map(|(cname, g)| g.nodes.get(&node_id).map(|n| (cname.as_str(), n)))
    }

    /// Find a node by ID across all canvases (mutable).
    pub fn find_node_mut(&mut self, node_id: NodeId) -> Option<&mut Node> {
        self.all_graphs.values_mut().find_map(|g| g.nodes.get_mut(&node_id))
    }

    /// Access the scheme engine through the actor runtime.
    pub fn engine(&self) -> &SchemeEngine {
        self.actor_runtime.engine()
    }
}

// --- Graph library initialization ---

impl GraphRuntime {
    /// Initialize ports from exports for one canvas.
    pub fn init_graph_libraries(&mut self, canvas_name: &str) {
        // Set script_outputs from exports
        if let Some(graph) = self.all_graphs.get_mut(canvas_name) {
            for node in graph.nodes.values_mut() {
                if !node.exports.is_empty() {
                    node.script_outputs = node.exports.iter()
                        .map(|name| PortDef { name: name.clone(), port_type: PortType::F64 })
                        .collect();
                }
            }
        }

        // Derive script_inputs from imports
        let input_map: HashMap<NodeId, Vec<PortDef>> = if let Some(graph) = self.all_graphs.get(canvas_name) {
            graph.nodes.keys().copied().collect::<Vec<_>>()
                .into_iter()
                .filter_map(|id| {
                    let inputs = graph.derive_inputs_for_node(id);
                    if inputs.is_empty() { None } else { Some((id, inputs)) }
                })
                .collect()
        } else {
            HashMap::new()
        };
        if let Some(graph) = self.all_graphs.get_mut(canvas_name) {
            for (id, inputs) in input_map {
                if let Some(node) = graph.nodes.get_mut(&id) {
                    node.script_inputs = inputs;
                }
            }
        }

        // Register stub libraries for this canvas
        if let Some(graph) = self.all_graphs.get(canvas_name) {
            self.actor_runtime.engine().register_stub_libraries(canvas_name, &graph.nodes);
        }
    }

    /// Initialize libraries for all canvases.
    pub fn init_all_libraries(&mut self) {
        let names: Vec<String> = self.all_graphs.keys().cloned().collect();
        for name in names {
            self.init_graph_libraries(&name);
        }
    }
}

// --- Compute ---

impl GraphRuntime {
    /// Apply the result of a computation to a node's ports and outputs.
    pub fn apply_compute_result(&mut self, node_id: NodeId, result: &ScriptResult) {
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

    /// Register a node's outputs as an R6RS library (canvas module-name).
    pub fn register_node_libraries(&self, node_id: NodeId) {
        if let Some((canvas_name, node)) = self.find_node_canvas(node_id) {
            let module_name = node.label.replace(' ', "-");
            if !module_name.is_empty() {
                self.actor_runtime.engine().register_node_library_named(
                    node_id, canvas_name, &module_name, &node.output_values,
                );
            }
        }
    }

    /// Auto-publish a node's values to the network and loopback to other canvases.
    pub fn auto_publish_node(&mut self, node_id: NodeId) {
        let info: Option<(String, String, String, HashMap<String, Value>)> = self.find_node_canvas(node_id)
            .and_then(|(canvas_name, node)| {
                if node.phantom { return None; }
                let module_name = node.label.replace(' ', "-");
                if node.exports.is_empty() || module_name.is_empty() { return None; }
                let mut values = node.output_values.clone();
                for (k, v) in &node.widget_values { values.insert(k.clone(), v.clone()); }
                // Include source code only if canvas allows sharing
                let share_code = self.all_graphs.get(canvas_name)
                    .map_or(true, |g| g.share_code);
                if share_code && !node.script_code.is_empty() {
                    values.insert("__source__".to_string(), Value::Str(node.script_code.clone()));
                }
                // Include user name for peer identification
                if !self.user_name.is_empty() {
                    values.insert("__peer_name__".to_string(), Value::Str(self.user_name.clone()));
                }
                Some((canvas_name.to_string(), canvas_name.to_string(), module_name, values))
            });
        if let Some((source_canvas, header_canvas, module_name, values)) = info {
            let channel = format!("{}/{}", header_canvas, module_name);
            {
                let mut store = self.net_values.lock().unwrap();
                store.insert(("local".to_string(), channel.clone()), values.clone());
            }
            self.net_handle.send(NetCommand::Publish { channel: channel.clone(), values: values.clone() });
            log::info!("auto-publish \"{}\"", channel);

            // Loopback: deliver to all OTHER canvases
            let other_canvases: Vec<String> = self.all_graphs.keys()
                .filter(|k| **k != source_canvas)
                .cloned().collect();
            for other_canvas in other_canvases {
                self.deliver_values(&other_canvas, &source_canvas, &module_name, &values);
            }
        }
    }

    /// Propagate computation to direct downstream nodes.
    pub fn propagate_downstream(&mut self, node_id: NodeId) {
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
                    self.actor_runtime.compute(did, n.clone(), template, inputs, self.db.clone());
                }
            }
        }
    }

    /// Deliver values to a specific canvas as if from a peer.
    /// `module_name` is the module name (not the full channel).
    /// `peer` is the source canvas name (loopback) or peer ID (network).
    /// Creates/updates phantom node, registers R6RS libraries, recomputes downstream.
    pub fn deliver_values(&mut self, canvas_key: &str, peer: &str, module_name: &str, values: &HashMap<String, Value>) {
        // Extract metadata before filtering
        let source_code = match values.get("__source__") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        };
        if let Some(Value::Str(name)) = values.get("__peer_name__") {
            if !name.is_empty() {
                self.peer_names.insert(peer.to_string(), name.clone());
            }
        }

        // Filter out __ metadata from values that go into phantom node outputs
        let node_values: HashMap<String, Value> = values.iter()
            .filter(|(k, _)| !k.starts_with("__"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let graph = match self.all_graphs.get(canvas_key) {
            Some(g) => g,
            None => return,
        };

        // Skip if this canvas has a local node with this module name
        let has_local = graph.nodes.values().any(|n| {
            !n.phantom && n.label.replace(' ', "-") == module_name
        });
        if has_local { return; }

        // Register as a remote template in the Node Library
        if let Some(ref code) = source_code {
            let display_name = self.peer_names.get(peer)
                .cloned()
                .unwrap_or_else(|| peer.to_string());
            let template_key = format!("{}/{}", peer, module_name);
            let outputs: Vec<PortDef> = node_values.keys()
                .map(|k| PortDef { name: k.clone(), port_type: PortType::F64 })
                .collect();
            self.registry.templates.insert(template_key, NodeTemplate {
                name: module_name.to_string(),
                category: display_name,
                path: None,
                inputs: Vec::new(),
                outputs,
                wasm_bytes: None,
                builtin: None,
                script_code: Some(code.clone()),
            });
        }

        let phantom_id = graph.nodes.iter()
            .find(|(_, n)| n.phantom && n.label == module_name)
            .map(|(&id, _)| id);

        let graph = self.all_graphs.get_mut(canvas_key).unwrap();
        let pid = if let Some(id) = phantom_id {
            if let Some(n) = graph.nodes.get_mut(&id) {
                n.output_values = node_values.clone();
                n.remote_peer = Some(peer.to_string());
                let mut sorted_keys: Vec<_> = node_values.keys().cloned().collect();
                sorted_keys.sort();
                n.script_outputs = sorted_keys.iter()
                    .map(|k| PortDef { name: k.clone(), port_type: PortType::F64 })
                    .collect();
            }
            id
        } else {
            let id = graph.next_node_id;
            graph.next_node_id += 1;
            let phantom_count = graph.nodes.values().filter(|n| n.phantom).count();
            let mut sorted_keys: Vec<_> = node_values.keys().cloned().collect();
            sorted_keys.sort();
            let node = Node {
                id,
                template_name: "Script".to_string(),
                label: module_name.to_string(),
                pos: [900.0, 50.0 + phantom_count as f32 * 200.0],
                input_values: HashMap::new(),
                output_values: node_values.clone(),
                script_code: String::new(),
                script_inputs: Vec::new(),
                script_outputs: sorted_keys.iter()
                    .map(|k| PortDef { name: k.clone(), port_type: PortType::F64 })
                    .collect(),
                widget_decls: Vec::new(),
                widget_values: HashMap::new(),
                exports: Vec::new(),
                imports: Vec::new(),
                code_hash: 0,
                error: None,
                last_exec_us: None,
                render_blocks: Vec::new(),
                phantom: true,
                remote_peer: Some(peer.to_string()),
            };
            graph.nodes.insert(id, node);
            log::info!(
                "created phantom node #{} \"{}\" from peer \"{}\" on canvas \"{}\"",
                id, module_name, peer, canvas_key
            );
            id
        };

        // Register R6RS library (peer module-name) for imports
        self.actor_runtime.engine().register_node_library_named(pid, peer, module_name, &node_values);

        // Recompute downstream nodes that import this module
        let module_str = module_name.to_string();
        let graph = self.all_graphs.get(canvas_key).unwrap();
        let downstream: Vec<NodeId> = graph.nodes.iter()
            .filter(|(_, n)| {
                if n.phantom { return false; }
                n.imports.iter().any(|(_, m)| *m == module_str)
            })
            .map(|(id, _)| *id).collect();
        for did in downstream {
            if !self.pending_nodes.contains(&did) {
                if let Some(n) = self.all_graphs.get(canvas_key).unwrap().nodes.get(&did) {
                    let template = self.registry.templates.get(&n.template_name).cloned();
                    let inputs = self.all_graphs.get(canvas_key).unwrap().resolve_all_input_values(did);
                    self.pending_nodes.insert(did);
                    self.actor_runtime.compute(did, n.clone(), template, inputs, self.db.clone());
                }
            }
        }
    }
}

// --- Compute orchestration ---

impl GraphRuntime {
    /// Before computing a node, ensure cross-canvas imports are available.
    /// For each unresolved import, search other canvases for a matching module
    /// and deliver its current values via loopback.
    pub fn ensure_imports(&mut self, node_id: NodeId) {
        let (canvas_name, node_imports) = match self.find_node_canvas(node_id) {
            Some((c, n)) => (c.to_string(), n.imports.clone()),
            None => return,
        };
        if node_imports.is_empty() { return; }
        let graph = match self.all_graphs.get(&canvas_name) {
            Some(g) => g,
            None => return,
        };

        // Collect import module names that have no matching node on this canvas
        let mut missing: Vec<String> = Vec::new();
        for (_, module_name) in &node_imports {
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
                    !n.phantom && n.label.replace(' ', "-") == *module_name
                }) {
                    let mut values = source_node.output_values.clone();
                    for (k, v) in &source_node.widget_values {
                        values.insert(k.clone(), v.clone());
                    }
                    if values.is_empty() {
                        for exp in &source_node.exports {
                            values.insert(exp.clone(), Value::F64(0.0));
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

    /// Resolve a node's data for computation: (Node, Option<Template>, inputs).
    pub fn resolve_node(&self, node_id: NodeId) -> Option<(Node, Option<NodeTemplate>, HashMap<String, Value>)> {
        let graph = self.all_graphs.values()
            .find(|g| g.nodes.contains_key(&node_id))?;
        let node = graph.nodes.get(&node_id)?;
        let template = self.registry.templates.get(&node.template_name).cloned();
        let available_inputs = graph.resolve_all_input_values(node_id);
        Some((node.clone(), template, available_inputs))
    }

    /// Compute a node if it is not already pending.
    pub fn compute_if_ready(&mut self, node_id: NodeId) {
        if self.pending_nodes.contains(&node_id) {
            return;
        }
        if self.find_node(node_id).map_or(false, |n| n.phantom) {
            return;
        }
        self.ensure_imports(node_id);
        if let Some((node, template, inputs)) = self.resolve_node(node_id) {
            self.pending_nodes.insert(node_id);
            self.actor_runtime.compute(node_id, node, template, inputs, self.db.clone());
        }
    }

    /// Compute all non-phantom nodes across all canvases in topological order.
    /// Used during peer startup to bring the graph up to date.
    pub fn compute_all(&mut self) {
        let canvas_names: Vec<String> = self.all_graphs.keys().cloned().collect();
        for canvas_name in canvas_names {
            let order = {
                let graph = match self.all_graphs.get(&canvas_name) {
                    Some(g) => g,
                    None => continue,
                };
                match graph.topological_sort() {
                    Ok(sorted) => sorted,
                    Err(fallback) => fallback,
                }
            };
            for node_id in order {
                let is_phantom = self.all_graphs.get(&canvas_name)
                    .and_then(|g| g.nodes.get(&node_id))
                    .map_or(true, |n| n.phantom);
                if is_phantom { continue; }
                self.ensure_imports(node_id);
                if let Some((node, template, inputs)) = self.resolve_node(node_id) {
                    self.pending_nodes.insert(node_id);
                    self.actor_runtime.compute(node_id, node, template, inputs, self.db.clone());
                }
            }
        }
    }

    /// Process nREPL commands from the command channel.
    pub fn poll_nrepl_commands(&mut self, rx: &CommandReceiver) {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                NreplCommand::CreateNode { canvas, label, code, exports, imports, reply } => {
                    let result = self.cmd_create_node(&canvas, &label, &code, exports, imports);
                    let _ = reply.send(result);
                }
                NreplCommand::DeleteNode { canvas, label, reply } => {
                    let result = self.cmd_delete_node(&canvas, &label);
                    let _ = reply.send(result);
                }
                NreplCommand::UpdateNode { canvas, label, code, exports, imports, reply } => {
                    let result = self.cmd_update_node(&canvas, &label, code, exports, imports);
                    let _ = reply.send(result);
                }
                NreplCommand::NodeState { canvas, label, reply } => {
                    let result = self.cmd_node_state(&canvas, &label);
                    let _ = reply.send(result);
                }
                NreplCommand::ComputeNode { canvas, label, reply } => {
                    let result = self.cmd_compute_node(&canvas, &label);
                    let _ = reply.send(result);
                }
            }
        }
    }

    fn cmd_create_node(&mut self, canvas: &str, label: &str, code: &str,
                       exports: Vec<String>, imports: Vec<(String, String)>) -> Result<String, String> {
        let graph = self.all_graphs.get_mut(canvas)
            .ok_or_else(|| format!("canvas '{}' not found", canvas))?;
        let id = graph.next_node_id;
        graph.next_node_id += 1;
        let mut node = Node {
            id,
            template_name: "Script".to_string(),
            label: label.to_string(),
            pos: [100.0, 100.0],
            input_values: HashMap::new(),
            output_values: HashMap::new(),
            script_code: code.to_string(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            exports,
            imports,
            code_hash: 0,
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
            phantom: false,
            remote_peer: None,
        };
        node.recompute_hash();
        let _ = persistence::save_node_file(canvas, &node);
        graph.nodes.insert(id, node);
        log::info!("nREPL: created node #{} '{}' on '{}'", id, label, canvas);
        Ok(id.to_string())
    }

    fn cmd_delete_node(&mut self, canvas: &str, label: &str) -> Result<(), String> {
        let graph = self.all_graphs.get_mut(canvas)
            .ok_or_else(|| format!("canvas '{}' not found", canvas))?;
        let node_id = graph.nodes.iter()
            .find(|(_, n)| n.label.replace(' ', "-") == label && !n.phantom)
            .map(|(&id, _)| id)
            .ok_or_else(|| format!("node '{}' not found on '{}'", label, canvas))?;
        persistence::delete_node_file(canvas, label);
        graph.nodes.remove(&node_id);
        log::info!("nREPL: deleted node '{}' from '{}'", label, canvas);
        Ok(())
    }

    fn cmd_update_node(&mut self, canvas: &str, label: &str,
                       code: Option<String>, exports: Option<Vec<String>>,
                       imports: Option<Vec<(String, String)>>) -> Result<(), String> {
        let graph = self.all_graphs.get_mut(canvas)
            .ok_or_else(|| format!("canvas '{}' not found", canvas))?;
        let node = graph.nodes.values_mut()
            .find(|n| n.label.replace(' ', "-") == label && !n.phantom)
            .ok_or_else(|| format!("node '{}' not found on '{}'", label, canvas))?;
        if let Some(code) = code {
            node.set_code(code);
            let _ = persistence::save_node_file(canvas, node);
        }
        if let Some(exports) = exports {
            node.exports = exports;
        }
        if let Some(imports) = imports {
            node.imports = imports;
        }
        Ok(())
    }

    fn cmd_node_state(&self, canvas: &str, label: &str) -> Option<nrepl::NodeState> {
        let graph = self.all_graphs.get(canvas)?;
        let node = graph.nodes.values()
            .find(|n| n.label.replace(' ', "-") == label)?;
        Some(nrepl::NodeState {
            code: node.script_code.clone(),
            exports: node.exports.clone(),
            imports: node.imports.clone(),
            outputs: node.output_values.iter()
                .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                .collect(),
            error: node.error.clone(),
        })
    }

    fn cmd_compute_node(&mut self, canvas: &str, label: &str) -> Result<(), String> {
        let graph = self.all_graphs.get(canvas)
            .ok_or_else(|| format!("canvas '{}' not found", canvas))?;
        let node_id = graph.nodes.iter()
            .find(|(_, n)| n.label.replace(' ', "-") == label && !n.phantom)
            .map(|(&id, _)| id)
            .ok_or_else(|| format!("node '{}' not found on '{}'", label, canvas))?;
        self.compute_if_ready(node_id);
        Ok(())
    }
}
