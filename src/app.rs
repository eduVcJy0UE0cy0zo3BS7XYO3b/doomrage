use crate::canvas::{self, CanvasState};
use crate::debug_log::DebugLog;
use crate::executor::Executor;
use crate::panels::{self, PanelAction, PanelState, ScriptViewMode};
use crate::persistence::{self, UndoHistory};
use crate::registry::NodeRegistry;
use crate::scheme_engine::parse_port_declarations;
use crate::theme;
use crate::types::*;
use crate::worker::{DeferredQueue, WorkRequest, WorkResult};
use std::collections::HashSet;
use std::path::PathBuf;

pub struct WasmCanvasApp {
    graph: Graph,
    registry: NodeRegistry,
    executor: Executor,
    worker: DeferredQueue,
    canvas_state: CanvasState,
    panel_state: PanelState,
    undo_history: UndoHistory,
    run_events: Vec<RunEvent>,
    auto_run: bool,
    graph_dirty: bool,
    current_file: Option<PathBuf>,
    theme_applied: bool,
    pending_nodes: HashSet<NodeId>,
    debug_log: DebugLog,
}

impl WasmCanvasApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let nodes_dir = PathBuf::from("./nodes");
        let mut registry = NodeRegistry::new(nodes_dir);
        if let Err(e) = registry.scan() {
            log::error!("Failed to scan nodes directory: {}", e);
        }

        let executor = Executor::new().expect("Failed to create WASM executor");
        let worker = DeferredQueue::new();

        // Restore DB state from previous session
        let db_auto_path = PathBuf::from("./db.json");
        if let Err(e) = persistence::load_db(&executor.db, &db_auto_path) {
            log::warn!("Failed to restore DB: {}", e);
        }

        // Try loading demo graph on first launch
        let demo_path = PathBuf::from("./demo.json");
        let graph = if demo_path.exists() {
            match persistence::load_graph(&demo_path, &executor.db) {
                Ok(g) => {
                    log::info!("Loaded demo graph");
                    g
                }
                Err(e) => {
                    log::warn!("Failed to load demo graph: {}", e);
                    Graph::new()
                }
            }
        } else {
            Graph::new()
        };
        // Sync ports for Script nodes loaded from file
        let mut graph = graph;
        for node in graph.nodes.values_mut() {
            if node.template_name == "Script" {
                Self::sync_script_ports(node);
            }
        }

        let mut undo_history = UndoHistory::new(10);
        undo_history.push(&graph);

        Self {
            graph,
            registry,
            executor,
            worker,
            canvas_state: CanvasState::new(),
            panel_state: PanelState::new(),
            undo_history,
            run_events: Vec::new(),
            auto_run: false,
            graph_dirty: false,
            current_file: None,
            theme_applied: false,
            pending_nodes: HashSet::new(),
            debug_log: DebugLog::new(),
        }
    }

    fn run_graph(&mut self) {
        let events = self.executor.execute_graph(&mut self.graph, &self.registry);
        self.run_events.extend(events);
        self.graph_dirty = false;
    }

    /// Parse (input)/(output) declarations from script code and sync node ports
    fn sync_script_ports(node: &mut Node) {
        use crate::scheme_engine::parse_port_declarations;

        let (input_decls, output_decls) = parse_port_declarations(&node.script_code);

        node.script_inputs = input_decls
            .iter()
            .map(|d| PortDef {
                name: d.name.clone(),
                port_type: PortType::from_str(&d.port_type).unwrap_or(PortType::F64),
            })
            .collect();

        node.script_outputs = output_decls
            .iter()
            .map(|d| PortDef {
                name: d.name.clone(),
                port_type: PortType::from_str(&d.port_type).unwrap_or(PortType::F64),
            })
            .collect();

        // Ensure input_values has defaults for new ports
        for port in &node.script_inputs {
            node.input_values
                .entry(port.name.clone())
                .or_insert_with(|| port.port_type.default_value());
        }
    }

    fn compute_node(&mut self, node_id: NodeId) {
        // First execute WASM upstream nodes synchronously (they're fast)
        let ancestors = self.graph.ancestors_sorted(node_id);
        for &aid in &ancestors {
            if aid == node_id {
                continue;
            }
            if let Some(node) = self.graph.nodes.get(&aid) {
                if node.template_name != "Script" {
                    // Execute non-script nodes synchronously
                    let events = self.executor.execute_up_to(&mut self.graph, &self.registry, aid);
                    self.run_events.extend(events);
                }
            }
        }

        // Now dispatch Script node to background worker
        if let Some(node) = self.graph.nodes.get(&node_id) {
            if node.template_name == "Script" {
                let code = node.script_code.clone();
                let (input_decls, output_decls) = parse_port_declarations(&code);

                let eff_inputs = node.effective_inputs(
                    self.registry.templates.get(&node.template_name).map(|t| t as &NodeTemplate),
                );
                let resolved = self.graph.resolve_input_values(node_id, eff_inputs);
                let bindings: Vec<(String, Value)> = input_decls
                    .iter()
                    .filter_map(|decl| {
                        let val = resolved.get(&decl.name)?;
                        Some((decl.name.clone(), val.clone()))
                    })
                    .collect();

                let output_names: Vec<String> = output_decls.iter().map(|d| d.name.clone()).collect();

                self.pending_nodes.insert(node_id);
                self.worker.send(WorkRequest::Compute {
                    node_id,
                    code,
                    input_bindings: bindings,
                    output_names,
                    db: self.executor.db.clone(),
                });
                return;
            }
        }

        // Fallback: non-script node
        let events = self.executor.execute_up_to(&mut self.graph, &self.registry, node_id);
        self.run_events.extend(events);
    }

    fn poll_worker_results(&mut self) {
        if let Some(result) = self.worker.poll(&self.executor.scheme) {
            match result {
                WorkResult::Preview { node_id, blocks } => {
                    self.pending_nodes.remove(&node_id);
                    if let Some(n) = self.graph.nodes.get_mut(&node_id) {
                        n.render_blocks = blocks;
                    }
                }
                WorkResult::Compute { node_id, result } => {
                    self.pending_nodes.remove(&node_id);
                    let label = self.graph.nodes.get(&node_id)
                        .map(|n| n.label.clone()).unwrap_or_default();
                    self.debug_log.log("compute", format!(
                        "#{} \"{}\" → {} blocks, {} outputs",
                        node_id, label,
                        result.render_blocks.len(),
                        result.output_values.len()
                    ));
                    if let Some(n) = self.graph.nodes.get_mut(&node_id) {
                        n.render_blocks = result.render_blocks;
                        n.error = None;
                        for (name, val) in &result.output_values {
                            n.output_values.insert(name.clone(), val.clone());
                        }
                    }
                }
                WorkResult::Error { node_id, message } => {
                    self.debug_log.log("error", format!("#{}: {}", node_id, &message));
                    self.pending_nodes.remove(&node_id);
                    if let Some(n) = self.graph.nodes.get_mut(&node_id) {
                        n.error = Some(message);
                    }
                }
            }
        }
    }

    fn handle_actions(&mut self, actions: Vec<PanelAction>) {
        for action in actions {
            match action {
                PanelAction::RunGraph => {
                    self.run_graph();
                }
                PanelAction::ComputeNode(id) => {
                    self.compute_node(id);
                }
                PanelAction::CancelCompute => {
                    self.worker.cancel();
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
                PanelAction::SyncScriptPorts(id) => {
                    if let Some(node) = self.graph.nodes.get_mut(&id) {
                        Self::sync_script_ports(node);
                    }
                }
                PanelAction::StepGraph => {
                    // TODO: step mode
                    self.run_graph();
                }
                PanelAction::ToggleAutoRun => {
                    self.auto_run = !self.auto_run;
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
                        let id = self.graph.add_node(&template, pos);
                        // Sync ports for Script nodes
                        if template.builtin == Some(BuiltinKind::Script) {
                            if let Some(node) = self.graph.nodes.get_mut(&id) {
                                Self::sync_script_ports(node);
                            }
                        }
                        self.undo_history.push(&self.graph);
                        self.graph_dirty = true;
                    }
                }
                PanelAction::DeleteNode(id) => {
                    self.graph.remove_node(id);
                    self.undo_history.push(&self.graph);
                    self.graph_dirty = true;
                    if self.panel_state.selected_node == Some(id) {
                        self.panel_state.selected_node = None;
                    }
                }
            }
        }
    }

    fn save_graph(&mut self) {
        let path = self.current_file.clone().or_else(|| {
            rfd::FileDialog::new()
                .set_title("Save Graph")
                .add_filter("JSON", &["json"])
                .save_file()
        });

        if let Some(path) = path {
            if let Err(e) = persistence::save_graph(&self.graph, &path, &self.executor.db) {
                log::error!("Failed to save graph: {}", e);
            } else {
                self.current_file = Some(path);
            }
        }
    }

    fn load_graph(&mut self) {
        let path = rfd::FileDialog::new()
            .set_title("Load Graph")
            .add_filter("JSON", &["json"])
            .pick_file();

        if let Some(path) = path {
            match persistence::load_graph(&path, &self.executor.db) {
                Ok(graph) => {
                    self.graph = graph;
                    self.undo_history.push(&self.graph);
                    self.current_file = Some(path);
                    self.run_events.clear();
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

        // Poll background worker results
        self.poll_worker_results();

        // Keep repainting while work is pending
        if !self.pending_nodes.is_empty() || self.worker.has_pending() {
            ctx.request_repaint();
        }

        // Keyboard shortcuts
        let mut actions = Vec::new();

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::R)) {
            actions.push(PanelAction::RunGraph);
        }
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
            let toolbar_actions = panels::draw_toolbar(ui, self.auto_run);
            actions.extend(toolbar_actions);
        });

        // Bottom panel - execution log or debug
        if self.panel_state.show_log || self.panel_state.show_debug {
            egui::TopBottomPanel::bottom("bottom_panel")
                .resizable(true)
                .default_height(150.0)
                .show(ctx, |ui| {
                    if self.panel_state.show_debug {
                        panels::draw_debug_panel(ui, &self.executor.db, &mut self.debug_log);
                    } else {
                        panels::draw_execution_log(ui, &self.run_events);
                    }
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
                    let insp_actions = panels::draw_inspector(
                        ui,
                        &mut self.graph,
                        &self.registry,
                        &mut self.panel_state,
                        &self.executor.db,
                        computing,
                        &mut self.debug_log,
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

            if let Some((from_node, from_port, to_node, to_port)) = canvas_response.new_connection
            {
                self.graph
                    .add_connection(from_node, from_port, to_node, to_port);
                self.undo_history.push(&self.graph);
                self.graph_dirty = true;
            }

            for node_id in canvas_response.delete_nodes {
                self.graph.remove_node(node_id);
                self.graph_dirty = true;
            }
            if self.graph_dirty {
                self.undo_history.push(&self.graph);
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

        // Auto-run
        if self.auto_run && self.graph_dirty {
            self.run_graph();
        }
    }

    fn on_exit(&mut self) {
        if let Err(e) = persistence::save_db(&self.executor.db, &PathBuf::from("./db.json")) {
            log::error!("Failed to auto-save DB: {}", e);
        }
    }
}
