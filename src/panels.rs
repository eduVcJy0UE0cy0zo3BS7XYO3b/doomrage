use crate::registry::NodeRegistry;
use crate::render::{PlotData, RenderBlock, StoreAction};
use crate::store::Store;
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
            selected_node: None,
            script_view: ScriptViewMode::Rendered,
        }
    }
}

pub enum PanelAction {
    AddNode(String, [f32; 2]),
    RunGraph,
    ComputeNode(NodeId),
    CancelCompute,
    SyncScriptPorts(NodeId),
    StepGraph,
    ToggleAutoRun,
    SaveGraph,
    LoadGraph,
    DeleteNode(NodeId),
}

pub fn draw_toolbar(ui: &mut egui::Ui, auto_run: bool) -> Vec<PanelAction> {
    let mut actions = Vec::new();

    ui.horizontal(|ui| {
        ui.visuals_mut().override_text_color = Some(TEXT);

        if ui
            .button(RichText::new("  Run  ").color(Color32::from_rgb(0x22, 0x8b, 0x22)))
            .clicked()
        {
            actions.push(PanelAction::RunGraph);
        }

        if ui
            .button(RichText::new(" Step ").color(ACCENT))
            .clicked()
        {
            actions.push(PanelAction::StepGraph);
        }

        let auto_label = if auto_run { " Auto [ON] " } else { " Auto [OFF] " };
        let auto_color = if auto_run {
            Color32::from_rgb(0x22, 0x8b, 0x22)
        } else {
            TEXT_DIM
        };
        if ui
            .button(RichText::new(auto_label).color(auto_color))
            .clicked()
        {
            actions.push(PanelAction::ToggleAutoRun);
        }

        ui.separator();

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
    store: &Store,
    is_computing: bool,
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
                            actions.push(PanelAction::SyncScriptPorts(node_id));
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

                        if is_computing {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Computing...").color(ACCENT));
                                if ui.button(RichText::new("Cancel").color(Color32::from_rgb(0xff, 0x44, 0x44))).clicked() {
                                    actions.push(PanelAction::CancelCompute);
                                }
                            });
                        } else if node.render_blocks.is_empty() {
                            ui.label(RichText::new("Press Compute to evaluate").color(TEXT_DIM));
                        } else {
                            egui::ScrollArea::vertical()
                                .id_salt("script_render_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    draw_render_blocks(ui, &node.render_blocks, store);
                                });
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

fn draw_render_blocks(ui: &mut egui::Ui, blocks: &[RenderBlock], store: &Store) {
    for (block_idx, block) in blocks.iter().enumerate() {
        ui.push_id(block_idx, |ui| {
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
                draw_render_blocks(ui, inner, store);
            }
            RenderBlock::Button { label, action } => {
                if ui.button(RichText::new(label).color(ACCENT)).clicked() {
                    match action {
                        StoreAction::Set { key, value } => {
                            store.set(key, Store::scheme_to_value(value));
                            let _ = store.save();
                        }
                        StoreAction::Append { key, value } => {
                            store.append(key, Store::scheme_to_value(value));
                            let _ = store.save();
                        }
                        StoreAction::Delete { key } => {
                            store.delete(key);
                            let _ = store.save();
                        }
                    }
                }
            }
            RenderBlock::Checkbox { label, key } => {
                ui.push_id(format!("cb_{}", key), |ui| {
                    let current = store
                        .get(key)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let mut checked = current;
                    if ui.checkbox(&mut checked, RichText::new(label).color(TEXT)).changed() {
                        store.set(key, serde_json::Value::Bool(checked));
                        let _ = store.save();
                    }
                });
            }
            RenderBlock::TextInput { key, placeholder } => {
                ui.push_id(format!("ti_{}", key), |ui| {
                    let current = store
                        .get(key)
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    let mut text = current;
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .hint_text(placeholder)
                            .desired_width(ui.available_width()),
                    );
                    if response.changed() {
                        store.set(key, serde_json::Value::String(text));
                        let _ = store.save();
                    }
                });
            }
            RenderBlock::Slider { key, min, max } => {
                ui.push_id(format!("sl_{}", key), |ui| {
                    let current = store
                        .get(key)
                        .and_then(|v| v.as_f64())
                        .unwrap_or(*min);
                    let mut val = current;
                    if ui.add(egui::Slider::new(&mut val, *min..=*max)).changed() {
                        store.set(
                            key,
                            serde_json::Number::from_f64(val)
                                .map(serde_json::Value::Number)
                                .unwrap_or(serde_json::Value::Null),
                    );
                    let _ = store.save();
                }
                });
            }
        }
        }); // push_id
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
