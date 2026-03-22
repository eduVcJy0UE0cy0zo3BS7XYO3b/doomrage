use crate::db::Db;
use crate::debug_log::DebugLog;
use crate::registry::NodeRegistry;
use crate::render::{DrawCmd, PlotData, RenderBlock, StoreAction};
use crate::theme::*;
use crate::types::*;
use egui::{Color32, CornerRadius, RichText, Sense};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptViewMode {
    Source,
    Rendered,
}

pub struct PanelState {
    pub library_search: String,
    pub show_library: bool,
    pub show_inspector: bool,
    pub show_log: bool,
    pub show_debug: bool,
    pub selected_node: Option<NodeId>,
    pub script_view: ScriptViewMode,
}

impl PanelState {
    pub fn new() -> Self {
        Self {
            library_search: String::new(),
            show_library: true,
            show_inspector: true,
            show_log: true,
            show_debug: false,
            selected_node: None,
            script_view: ScriptViewMode::Rendered,
        }
    }
}

pub enum PanelAction {
    AddNode(String, [f32; 2]),
    ComputeNode(NodeId),
    CancelCompute,
    RecomputeSelected,
    SaveGraph,
    LoadGraph,
    DeleteNode(NodeId),
    UpdateWidget(NodeId, String, Value),
}

pub fn draw_toolbar(ui: &mut egui::Ui) -> Vec<PanelAction> {
    let mut actions = Vec::new();

    ui.horizontal(|ui| {
        ui.visuals_mut().override_text_color = Some(TEXT);

        if ui.button(RichText::new(" Save ").color(TEXT)).clicked()
            || ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S))
        {
            actions.push(PanelAction::SaveGraph);
        }

        if ui.button(RichText::new(" Load ").color(TEXT)).clicked()
            || ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::O))
        {
            actions.push(PanelAction::LoadGraph);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("WASM Canvas").color(ACCENT).strong());
        });
    });

    actions
}

pub fn draw_library(
    ui: &mut egui::Ui,
    registry: &NodeRegistry,
    panel: &mut PanelState,
) -> Vec<PanelAction> {
    let mut actions = Vec::new();

    ui.vertical(|ui| {
        ui.label(RichText::new("Node Library").color(ACCENT).strong());
        ui.separator();

        // Search
        let search_response = ui.add(
            egui::TextEdit::singleline(&mut panel.library_search)
                .hint_text("Search nodes...")
                .desired_width(ui.available_width()),
        );

        ui.add_space(4.0);

        let search = panel.library_search.to_lowercase();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (category, templates) in registry.grouped_templates() {
                    let filtered: Vec<_> = templates
                        .iter()
                        .filter(|t| search.is_empty() || t.name.to_lowercase().contains(&search))
                        .collect();

                    if filtered.is_empty() {
                        continue;
                    }

                    ui.label(RichText::new(&category).color(TEXT_DIM).small());
                    ui.add_space(2.0);

                    for template in filtered {
                        let accent = node_accent_color(&template.name);
                        let response = ui.horizontal(|ui| {
                            // Color indicator
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(2), accent);

                            let label = ui.label(
                                RichText::new(&template.name).color(TEXT),
                            );

                            // Port count hint
                            let hint = format!(
                                "{}in {}out",
                                template.inputs.len(),
                                template.outputs.len()
                            );
                            ui.label(RichText::new(hint).color(TEXT_DIM).small());

                            label
                        });

                        if response.inner.double_clicked() {
                            actions.push(PanelAction::AddNode(
                                template.name.clone(),
                                [100.0, 100.0],
                            ));
                        }
                    }

                    ui.add_space(4.0);
                }
            });
    });

    actions
}

pub fn draw_inspector(
    ui: &mut egui::Ui,
    graph: &mut Graph,
    registry: &NodeRegistry,
    panel: &mut PanelState,
    db: &Db,
    is_computing: bool,
    debug_log: &mut DebugLog,
) -> Vec<PanelAction> {
    let mut actions = Vec::new();

    let node_id = match panel.selected_node {
        Some(id) if graph.nodes.contains_key(&id) => id,
        _ => {
            ui.label(RichText::new("No node selected").color(TEXT_DIM));
            return actions;
        }
    };

    ui.vertical(|ui| {
        ui.label(RichText::new("Inspector").color(ACCENT).strong());
        ui.separator();

        let node = graph.nodes.get(&node_id).unwrap().clone();
        let template = registry.templates.get(&node.template_name);

        // Node info
        ui.label(RichText::new(format!("#{} {}", node.id, node.template_name)).color(TEXT));

        if let Some(template) = template {
            if template.builtin == Some(BuiltinKind::Const) {
                // Const node: value editor
                ui.add_space(4.0);
                ui.label(RichText::new("Value").color(TEXT_DIM));

                let mut val = graph.nodes[&node_id]
                    .input_values
                    .get("value")
                    .cloned()
                    .unwrap_or(Value::F64(0.0));

                let changed = draw_value_editor(ui, &mut val);
                if changed {
                    graph
                        .nodes
                        .get_mut(&node_id)
                        .unwrap()
                        .input_values
                        .insert("value".to_string(), val);
                }
            } else if template.builtin == Some(BuiltinKind::Script) {
                // Mode toggle
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let src_color = if panel.script_view == ScriptViewMode::Source { ACCENT } else { TEXT_DIM };
                    let ren_color = if panel.script_view == ScriptViewMode::Rendered { ACCENT } else { TEXT_DIM };
                    if ui.button(RichText::new("Source").color(src_color)).clicked() {
                        panel.script_view = ScriptViewMode::Source;
                    }
                    if ui.button(RichText::new("Rendered").color(ren_color)).clicked() {
                        panel.script_view = ScriptViewMode::Rendered;
                    }
                });
                ui.separator();

                match panel.script_view {
                    ScriptViewMode::Source => {
                        let mut code = graph.nodes[&node_id].script_code.clone();
                        let response = ui.add(
                            egui::TextEdit::multiline(&mut code)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(ui.available_width())
                                .desired_rows(12)
                                .code_editor(),
                        );
                        if response.changed() {
                            let n = graph.nodes.get_mut(&node_id).unwrap();
                            n.script_code = code;
                            n.render_blocks.clear();
                        }

                        // Input port overrides
                        ui.add_space(4.0);
                        ui.label(RichText::new("Inputs").color(TEXT_DIM));
                        for port in &template.inputs {
                            let connected = graph.input_connection(node_id, &port.name).is_some();
                            if connected {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&port.name).color(TEXT_DIM));
                                    ui.label(RichText::new("(connected)").color(TEXT_DIM).small());
                                });
                            } else {
                                ui.label(RichText::new(&port.name).color(TEXT));
                                let mut val = graph.nodes[&node_id]
                                    .input_values
                                    .get(&port.name)
                                    .cloned()
                                    .unwrap_or_else(|| port.port_type.default_value());
                                if draw_value_editor(ui, &mut val) {
                                    graph
                                        .nodes
                                        .get_mut(&node_id)
                                        .unwrap()
                                        .input_values
                                        .insert(port.name.clone(), val);
                                }
                            }
                        }
                    }
                    ScriptViewMode::Rendered => {
                        if ui
                            .button(RichText::new("  Compute  ").color(Color32::from_rgb(0x22, 0x8b, 0x22)))
                            .clicked()
                        {
                            actions.push(PanelAction::ComputeNode(node_id));
                        }
                        ui.add_space(4.0);

                        if is_computing && node.render_blocks.is_empty() && node.widget_decls.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Computing...").color(ACCENT));
                                if ui.button(RichText::new("Cancel").color(Color32::from_rgb(0xff, 0x44, 0x44))).clicked() {
                                    actions.push(PanelAction::CancelCompute);
                                }
                            });
                        } else if !is_computing && node.render_blocks.is_empty() && node.widget_decls.is_empty() {
                            ui.label(RichText::new("Press Compute to evaluate").color(TEXT_DIM));
                        } else {
                            // Draw new-style widgets from widget_decls
                            for wdecl in &node.widget_decls {
                                let current = graph.nodes.get(&node_id).unwrap()
                                    .widget_values.get(&wdecl.name)
                                    .map(|v| v.as_f64())
                                    .unwrap_or(wdecl.params.first().copied().unwrap_or(0.0));
                                match wdecl.widget_type.as_str() {
                                    "slider" => {
                                        let min = wdecl.params.first().copied().unwrap_or(0.0);
                                        let max = wdecl.params.get(1).copied().unwrap_or(100.0);
                                        let mut val = current;
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new(&wdecl.name).color(TEXT_DIM));
                                            if ui.add(egui::Slider::new(&mut val, min..=max)).changed() {
                                                actions.push(PanelAction::UpdateWidget(
                                                    node_id, wdecl.name.clone(), Value::F64(val),
                                                ));
                                            }
                                        });
                                    }
                                    "checkbox" => {
                                        let mut checked = current != 0.0;
                                        if ui.checkbox(&mut checked, RichText::new(&wdecl.name).color(TEXT)).changed() {
                                            actions.push(PanelAction::UpdateWidget(
                                                node_id, wdecl.name.clone(),
                                                Value::F64(if checked { 1.0 } else { 0.0 }),
                                            ));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if !node.widget_decls.is_empty() && !node.render_blocks.is_empty() {
                                ui.separator();
                            }
                            if draw_render_blocks(ui, &node.render_blocks, db, debug_log) {
                                actions.push(PanelAction::RecomputeSelected);
                            }
                        }
                    }
                }
            } else {
                // Input port overrides
                ui.add_space(4.0);
                ui.label(RichText::new("Inputs").color(TEXT_DIM));

                for port in &template.inputs {
                    let connected = graph.input_connection(node_id, &port.name).is_some();
                    if connected {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&port.name).color(TEXT_DIM));
                            ui.label(RichText::new("(connected)").color(TEXT_DIM).small());
                        });
                    } else {
                        ui.label(RichText::new(&port.name).color(TEXT));
                        let mut val = graph.nodes[&node_id]
                            .input_values
                            .get(&port.name)
                            .cloned()
                            .unwrap_or_else(|| port.port_type.default_value());

                        if draw_value_editor(ui, &mut val) {
                            graph
                                .nodes
                                .get_mut(&node_id)
                                .unwrap()
                                .input_values
                                .insert(port.name.clone(), val);
                        }
                    }
                }
            }

            // Outputs display
            if !node.output_values.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("Outputs").color(TEXT_DIM));
                for (name, val) in &node.output_values {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(name).color(TEXT_DIM));
                        ui.label(RichText::new(val.display()).color(ACCENT).monospace());
                    });
                }
            }

            // Execution info
            if let Some(us) = node.last_exec_us {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Last run: {}us", us))
                        .color(TEXT_DIM)
                        .small(),
                );
            }

            // Error
            if let Some(err) = &node.error {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Error: {}", err))
                        .color(Color32::from_rgb(0xff, 0x44, 0x44))
                        .small(),
                );
            }

            // WASM info
            if template.builtin.is_none() {
                ui.add_space(4.0);
                if let Some(bytes) = &template.wasm_bytes {
                    ui.label(
                        RichText::new(format!("Component size: {} bytes", bytes.len()))
                            .color(TEXT_DIM)
                            .small(),
                    );
                }
            }
        }

        ui.add_space(8.0);
        if ui
            .button(RichText::new("Delete Node").color(Color32::from_rgb(0xff, 0x44, 0x44)))
            .clicked()
        {
            actions.push(PanelAction::DeleteNode(node_id));
        }
    });

    actions
}

fn draw_value_editor(ui: &mut egui::Ui, val: &mut Value) -> bool {
    let mut changed = false;
    match val {
        Value::F64(v) => {
            let response = ui.add(egui::DragValue::new(v).speed(0.1));
            changed = response.changed();
        }
        Value::F32(v) => {
            let mut f = *v as f64;
            let response = ui.add(egui::DragValue::new(&mut f).speed(0.1));
            if response.changed() {
                *v = f as f32;
                changed = true;
            }
        }
        Value::I64(v) => {
            let response = ui.add(egui::DragValue::new(v).speed(1.0));
            changed = response.changed();
        }
        Value::I32(v) => {
            let response = ui.add(egui::DragValue::new(v).speed(1.0));
            changed = response.changed();
        }
        Value::Bool(v) => {
            changed = ui.checkbox(v, "").changed();
        }
        Value::Str(v) => {
            changed = ui
                .add(egui::TextEdit::singleline(v).desired_width(ui.available_width()))
                .changed();
        }
    }
    changed
}

pub fn draw_execution_log(ui: &mut egui::Ui, events: &[RunEvent]) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Execution Log").color(ACCENT).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |_ui| {
            // Clear and copy handled by caller
        });
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for event in events {
                let (color, status) = match &event.result {
                    Ok(preview) => (Color32::from_rgb(0x22, 0x8b, 0x22), preview.as_str()),
                    Err(err) => (Color32::from_rgb(0xff, 0x44, 0x44), err.as_str()),
                };

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("#{}", event.node_id))
                            .color(TEXT_DIM)
                            .monospace()
                            .small(),
                    );
                    ui.label(
                        RichText::new(&event.node_name)
                            .color(TEXT)
                            .small(),
                    );
                    ui.label(
                        RichText::new(format!("{}us", event.duration_us))
                            .color(TEXT_DIM)
                            .monospace()
                            .small(),
                    );
                    ui.label(
                        RichText::new(status)
                            .color(color)
                            .monospace()
                            .small(),
                    );
                });
            }
        });
}

/// Returns true if db was mutated (needs recompute)
pub fn draw_render_blocks(ui: &mut egui::Ui, blocks: &[RenderBlock], db: &Db, debug_log: &mut DebugLog) -> bool {
    let mut store_mutated = false;
    for (block_idx, block) in blocks.iter().enumerate() {
        let stable_id = format!("rb_{}_{}", block_idx, block_id_hint(block));
        ui.push_id(stable_id, |ui| {
        match block {
            RenderBlock::Text(t) => {
                ui.label(RichText::new(t).color(TEXT));
            }
            RenderBlock::Bold(t) => {
                ui.label(RichText::new(t).color(ACCENT).strong());
            }
            RenderBlock::Italic(t) => {
                ui.label(RichText::new(t).color(TEXT).italics());
            }
            RenderBlock::Code(t) => {
                egui::Frame::NONE
                    .fill(Color32::from_rgb(0xf0, 0xf0, 0xec))
                    .corner_radius(CornerRadius::same(3))
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(t)
                                .monospace()
                                .color(Color32::from_rgb(0x22, 0x8b, 0x22)),
                        );
                    });
            }
            RenderBlock::Link { url, label } => {
                ui.hyperlink_to(
                    RichText::new(label).color(Color32::from_rgb(0x00, 0x66, 0xcc)),
                    url,
                );
            }
            RenderBlock::Hr => {
                ui.separator();
            }
            RenderBlock::Table { headers, rows } => {
                egui::Frame::NONE
                    .fill(Color32::from_rgb(0xf0, 0xf0, 0xec))
                    .corner_radius(CornerRadius::same(3))
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        egui::Grid::new("render_table")
                            .striped(true)
                            .min_col_width(40.0)
                            .show(ui, |ui| {
                                for h in headers {
                                    ui.label(RichText::new(h).color(ACCENT).strong().small());
                                }
                                ui.end_row();
                                for row in rows {
                                    for cell in row {
                                        ui.label(RichText::new(cell).color(TEXT).small());
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            }
            RenderBlock::Plot(plot_data) => {
                draw_plot(ui, plot_data);
            }
            RenderBlock::Group(inner) => {
                if draw_render_blocks(ui, inner, db, debug_log) {
                    store_mutated = true;
                }
            }
            RenderBlock::Button { label, action } => {
                if ui.button(RichText::new(label).color(ACCENT)).clicked() {
                    match action {
                        StoreAction::Set { key, value } => {
                            let resolved = resolve_store_ref(value, db);
                            debug_log.log("button", format!("set {:?} = {:?} (from {:?})", key, resolved, value));
                            db.kv_set(key, serde_json::Value::String(resolved));
                        }
                        StoreAction::Append { key, value } => {
                            let resolved = resolve_store_ref(value, db);
                            debug_log.log("button", format!("append {:?} ← {:?} (from {:?})", key, resolved, value));
                            if !resolved.is_empty() {
                                db.kv_append(key, serde_json::Value::String(resolved));
                            }
                        }
                        StoreAction::Delete { key } => {
                            debug_log.log("button", format!("delete {:?}", key));
                            db.kv_delete(key);
                        }
                    }
                    store_mutated = true;
                }
            }
            RenderBlock::Checkbox { label, key } => {
                let id = egui::Id::new(format!("wg_cb_{}", key));
                let mut checked = ui.ctx().data_mut(|d| *d.get_temp_mut_or(id, false));
                let resp = ui.checkbox(&mut checked, RichText::new(label).color(TEXT));
                ui.ctx().data_mut(|d| d.insert_temp(id, checked));
                if resp.changed() {
                    db.kv_set(key, serde_json::Value::Bool(checked));
                }
            }
            RenderBlock::TextInput { key, placeholder } => {
                let id = egui::Id::new(format!("wg_ti_{}", key));
                let mut is_new = false;
                let mut text = ui.ctx().data_mut(|d| {
                    d.get_temp_mut_or_insert_with(id, || {
                        is_new = true;
                        db.kv_get(key)
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .unwrap_or_default()
                    }).clone()
                });
                // Ensure kv key exists on first render so buttons can resolve it
                if is_new && db.kv_get(key).is_none() {
                    db.kv_set(key, serde_json::Value::String(text.clone()));
                }
                let response = ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .hint_text(placeholder)
                        .desired_width(ui.available_width()),
                );
                if response.changed() {
                    ui.ctx().data_mut(|d| d.insert_temp(id, text.clone()));
                    db.kv_set(key, serde_json::Value::String(text));
                }
            }
            RenderBlock::EditableList { key } => {
                let items = db
                    .kv_get(key)
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();

                let mut to_delete: Option<usize> = None;

                for (i, item) in items.iter().enumerate() {
                    let item_str = item.as_str().unwrap_or("").to_string();
                    let item_id = egui::Id::new(format!("el_{}_{}", key, i));

                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}.", i + 1)).color(TEXT_DIM));

                        let mut text = ui.ctx().data_mut(|d| {
                            d.get_temp_mut_or_insert_with(item_id, || item_str.clone()).clone()
                        });

                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut text)
                                .desired_width(150.0),
                        );
                        if resp.changed() {
                            ui.ctx().data_mut(|d| d.insert_temp(item_id, text.clone()));
                            let mut arr = items.clone();
                            arr[i] = serde_json::Value::String(text);
                            db.kv_set(key, serde_json::Value::Array(arr));
                            store_mutated = true;
                        }

                        if ui.button(RichText::new("x").color(Color32::from_rgb(0xff, 0x44, 0x44))).clicked() {
                            to_delete = Some(i);
                        }
                    });
                }

                if let Some(idx) = to_delete {
                    let mut arr = items.clone();
                    arr.remove(idx);
                    db.kv_set(key, serde_json::Value::Array(arr));
                    for i in 0..20 {
                        let id = egui::Id::new(format!("el_{}_{}", key, i));
                        ui.ctx().data_mut(|d| d.remove_temp::<String>(id));
                    }
                    store_mutated = true;
                }

                if items.is_empty() {
                    ui.label(RichText::new("(empty list)").color(TEXT_DIM).italics());
                }
            }
            RenderBlock::Canvas { width, height, commands } => {
                draw_canvas_block(ui, *width, *height, commands);
            }
            RenderBlock::Slider { key, min, max } => {
                let current = db
                    .kv_get(key)
                    .and_then(|v| v.as_f64())
                    .unwrap_or(*min);
                let mut val = current;
                if ui.add(egui::Slider::new(&mut val, *min..=*max)).changed() {
                    db.kv_set(
                        key,
                        serde_json::Number::from_f64(val)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
        }
        }); // push_id
    }
    store_mutated
}

/// If value is a kv key, resolve to current value; otherwise return as-is
fn resolve_store_ref(value: &str, db: &Db) -> String {
    if let Some(val) = db.kv_get(value) {
        if let Some(s) = val.as_str() {
            return s.to_string();
        }
        return format!("{}", val);
    }
    value.to_string()
}

fn block_id_hint(block: &RenderBlock) -> String {
    match block {
        RenderBlock::Checkbox { key, .. } => format!("cb_{}", key),
        RenderBlock::TextInput { key, .. } => format!("ti_{}", key),
        RenderBlock::Slider { key, .. } => format!("sl_{}", key),
        RenderBlock::Button { label, .. } => format!("btn_{}", label),
        RenderBlock::Table { .. } => "table".to_string(),
        RenderBlock::Plot(_) => "plot".to_string(),
        RenderBlock::Bold(t) => format!("b_{}", &t[..t.len().min(8)]),
        RenderBlock::Text(t) => format!("t_{}", &t[..t.len().min(8)]),
        RenderBlock::Canvas { .. } => "canvas".to_string(),
        _ => "x".to_string(),
    }
}

fn parse_hex_color(s: &str) -> Color32 {
    let s = s.trim_start_matches('#');
    if s.len() >= 6 {
        let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(128);
        let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(128);
        let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(128);
        Color32::from_rgb(r, g, b)
    } else {
        Color32::from_rgb(128, 128, 128)
    }
}

fn draw_canvas_block(ui: &mut egui::Ui, width: f64, height: f64, commands: &[DrawCmd]) {
    let size = egui::vec2(width as f32, height as f32);
    let (response, painter) = ui.allocate_painter(size, Sense::hover());
    let origin = response.rect.min;

    for cmd in commands {
        match cmd {
            DrawCmd::Line { x1, y1, x2, y2, color, width } => {
                painter.line_segment(
                    [
                        origin + egui::vec2(*x1 as f32, *y1 as f32),
                        origin + egui::vec2(*x2 as f32, *y2 as f32),
                    ],
                    egui::Stroke::new(*width as f32, parse_hex_color(color)),
                );
            }
            DrawCmd::Rect { x, y, w, h, fill } => {
                let rect = egui::Rect::from_min_size(
                    origin + egui::vec2(*x as f32, *y as f32),
                    egui::vec2(*w as f32, *h as f32),
                );
                painter.rect_filled(rect, CornerRadius::ZERO, parse_hex_color(fill));
            }
            DrawCmd::Circle { x, y, r, fill } => {
                painter.circle_filled(
                    origin + egui::vec2(*x as f32, *y as f32),
                    *r as f32,
                    parse_hex_color(fill),
                );
            }
            DrawCmd::Polyline { points, color, width } => {
                if points.len() >= 2 {
                    let pts: Vec<egui::Pos2> = points.iter()
                        .map(|p| origin + egui::vec2(p[0] as f32, p[1] as f32))
                        .collect();
                    let stroke = egui::Stroke::new(*width as f32, parse_hex_color(color));
                    for pair in pts.windows(2) {
                        painter.line_segment([pair[0], pair[1]], stroke);
                    }
                }
            }
            DrawCmd::Text { x, y, text, color, size } => {
                let font = egui::FontId::proportional(*size as f32);
                painter.text(
                    origin + egui::vec2(*x as f32, *y as f32),
                    egui::Align2::LEFT_TOP,
                    text,
                    font,
                    parse_hex_color(color),
                );
            }
        }
    }
}

fn draw_plot(ui: &mut egui::Ui, plot_data: &PlotData) {
    use egui_plot::{Bar, BarChart, Line, Plot, PlotPoints};

    let plot_height = 150.0;

    match plot_data {
        PlotData::Line { y, title } => {
            let points: PlotPoints = y
                .iter()
                .enumerate()
                .map(|(i, &v)| [i as f64, v])
                .collect();
            let plot_title = title.as_deref().unwrap_or("line");
            let line = Line::new(plot_title, points).color(ACCENT).width(2.0);

            Plot::new(format!("plot_{}", plot_title))
                .height(plot_height)
                .width(ui.available_width())
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .show_axes(true)
                .show(ui, |plot_ui| {
                    plot_ui.line(line);
                });
        }
        PlotData::Scatter { x, y, title } => {
            let points: PlotPoints = x
                .iter()
                .zip(y.iter())
                .map(|(&xi, &yi)| [xi, yi])
                .collect();
            let plot_title = title.as_deref().unwrap_or("scatter");
            let line = Line::new(plot_title, points)
                .color(Color32::from_rgb(0xff, 0xaa, 0x00))
                .width(2.0);

            Plot::new(format!("plot_{}", plot_title))
                .height(plot_height)
                .width(ui.available_width())
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .show(ui, |plot_ui| {
                    plot_ui.line(line);
                });
        }
        PlotData::Bar {
            labels: _,
            values,
            title,
        } => {
            let bars: Vec<Bar> = values
                .iter()
                .enumerate()
                .map(|(i, &v)| Bar::new(i as f64, v).width(0.8))
                .collect();
            let plot_title = title.as_deref().unwrap_or("bar");
            let chart = BarChart::new(plot_title, bars).color(ACCENT);

            Plot::new(format!("plot_{}", plot_title))
                .height(plot_height)
                .width(ui.available_width())
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .show(ui, |plot_ui| {
                    plot_ui.bar_chart(chart);
                });
        }
    }
}

pub fn draw_debug_panel(ui: &mut egui::Ui, db: &Db, debug_log: &mut DebugLog) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Debug").color(ACCENT).strong());
        if ui.button(RichText::new("Clear").color(TEXT_DIM).small()).clicked() {
            debug_log.clear();
        }
    });
    ui.separator();

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                // Left: DB state
                ui.vertical(|ui| {
                    ui.set_min_width(250.0);
                    ui.label(RichText::new("kv state").color(TEXT_DIM).small());
                    ui.separator();
                    let pairs = db.kv_all();
                    if pairs.is_empty() {
                        ui.label(RichText::new("(empty)").color(TEXT_DIM).small());
                    }
                    for (key, value) in &pairs {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(key).color(ACCENT).monospace().small());
                            let val_str = match value {
                                serde_json::Value::String(s) => format!("\"{}\"", s),
                                other => format!("{}", other),
                            };
                            let truncated = if val_str.len() > 60 {
                                format!("{}...", &val_str[..57])
                            } else {
                                val_str
                            };
                            ui.label(RichText::new(truncated).color(TEXT).monospace().small());
                        });
                    }
                });

                ui.separator();

                // Right: Event log
                ui.vertical(|ui| {
                    ui.label(RichText::new("Event log").color(TEXT_DIM).small());
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for entry in debug_log.entries() {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("{:>6}ms", entry.elapsed_ms))
                                            .color(TEXT_DIM)
                                            .monospace()
                                            .small(),
                                    );
                                    let source_color = match entry.source {
                                        "error" => Color32::from_rgb(0xff, 0x44, 0x44),
                                        "mutation" => Color32::from_rgb(0x22, 0x8b, 0x22),
                                        "button" => ACCENT,
                                        _ => TEXT_DIM,
                                    };
                                    ui.label(
                                        RichText::new(entry.source)
                                            .color(source_color)
                                            .monospace()
                                            .small(),
                                    );
                                    ui.label(
                                        RichText::new(&entry.message)
                                            .color(TEXT)
                                            .monospace()
                                            .small(),
                                    );
                                });
                            }
                        });
                });
            });
        });
}
