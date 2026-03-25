use crate::registry::NodeRegistry;
use crate::render::DrawCmd;
use crate::theme::*;
use crate::types::*;
use egui::{
    pos2, vec2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Sense, Stroke,
    StrokeKind, Vec2,
};

const NODE_WIDTH: f32 = 180.0;
const SCRIPT_NODE_WIDTH: f32 = 200.0;
const PORT_RADIUS: f32 = 5.0;
const HEADER_HEIGHT: f32 = 28.0;
const PORT_ROW_HEIGHT: f32 = 22.0;
const RESULT_HEIGHT: f32 = 24.0;
const ACCENT_STRIP_WIDTH: f32 = 4.0;

#[derive(Debug, Clone)]
pub enum DragKind {
    Node(NodeId, Vec2),
    BoxSelect(Pos2),
    None,
}

pub struct CanvasState {
    pub selected_nodes: Vec<NodeId>,
    pub drag: DragKind,
    pub context_menu_pos: Option<Pos2>,
    pub show_context_menu: bool,
}

impl CanvasState {
    pub fn new() -> Self {
        Self {
            selected_nodes: Vec::new(),
            drag: DragKind::None,
            context_menu_pos: None,
            show_context_menu: false,
        }
    }
}

fn cr(r: f32) -> CornerRadius {
    CornerRadius::same(r.round().max(0.0) as u8)
}

pub fn draw_canvas(
    ui: &mut egui::Ui,
    graph: &mut Graph,
    registry: &NodeRegistry,
    state: &mut CanvasState,
) -> CanvasResponse {
    let mut response = CanvasResponse::default();

    let (canvas_rect, canvas_response) =
        ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());

    let painter = ui.painter_at(canvas_rect);

    // Background
    painter.rect_filled(canvas_rect, CornerRadius::ZERO, BG);

    // Handle zoom
    let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
    if canvas_rect.contains(ui.input(|i| i.pointer.hover_pos().unwrap_or_default()))
        && scroll_delta != 0.0
    {
        let old_zoom = graph.viewport_zoom;
        graph.viewport_zoom =
            (graph.viewport_zoom * (1.0 + scroll_delta * 0.002)).clamp(0.2, 3.0);
        let new_zoom = graph.viewport_zoom;

        if let Some(cursor) = ui.input(|i| i.pointer.hover_pos()) {
            let cursor_in_canvas = cursor - canvas_rect.left_top().to_vec2();
            let scale_change = new_zoom / old_zoom;
            graph.viewport_offset[0] = cursor_in_canvas.x
                - (cursor_in_canvas.x - graph.viewport_offset[0]) * scale_change;
            graph.viewport_offset[1] = cursor_in_canvas.y
                - (cursor_in_canvas.y - graph.viewport_offset[1]) * scale_change;
        }
    }

    // Handle pan (middle mouse)
    if canvas_response.dragged_by(egui::PointerButton::Middle) {
        let delta = canvas_response.drag_delta();
        graph.viewport_offset[0] += delta.x;
        graph.viewport_offset[1] += delta.y;
    }

    // Space + left drag or Ctrl + left drag for pan
    if (ui.input(|i| i.key_down(egui::Key::Space)) || ui.input(|i| i.modifiers.ctrl))
        && canvas_response.dragged_by(egui::PointerButton::Primary)
    {
        let delta = canvas_response.drag_delta();
        graph.viewport_offset[0] += delta.x;
        graph.viewport_offset[1] += delta.y;
    }

    let offset = Vec2::new(graph.viewport_offset[0], graph.viewport_offset[1]);
    let zoom = graph.viewport_zoom;

    // Draw grid
    draw_grid(&painter, canvas_rect, offset, zoom);

    // Draw dependency wires (derived from imports in code)
    let derived = graph.derived_connections();
    for conn in &derived {
        if let (Some(from_node), Some(to_node)) = (
            graph.nodes.get(&conn.from_node),
            graph.nodes.get(&conn.to_node),
        ) {
            let from_template = registry.templates.get(&from_node.template_name);
            let to_template = registry.templates.get(&to_node.template_name);

            let (fp, tp) = match (&conn.from_port, &conn.to_port) {
                (Some(fp), Some(tp)) => (fp.as_str(), tp.as_str()),
                _ => continue, // skip non-port connections
            };
            let from_pos = match port_screen_pos(from_node, from_template, fp, true, canvas_rect, offset, zoom) {
                Some(p) => p,
                None => continue,
            };
            let to_pos = match port_screen_pos(to_node, to_template, tp, false, canvas_rect, offset, zoom) {
                Some(p) => p,
                None => continue,
            };
            let color = from_node.script_outputs.iter()
                .find(|p| p.name == fp)
                .map(|p| p.port_type.color())
                .unwrap_or(PortType::F64.color());

            draw_wire(&painter, from_pos, to_pos, color, false);
        }
    }


    // Draw nodes (sorted by selection for z-order)
    let mut node_ids: Vec<NodeId> = graph.nodes.keys().copied().collect();
    node_ids.sort_by_key(|id| {
        if state.selected_nodes.contains(id) {
            1
        } else {
            0
        }
    });

    let pointer_pos = ui.input(|i| i.pointer.hover_pos());

    for &node_id in &node_ids {
        let node = match graph.nodes.get(&node_id) {
            Some(n) => n,
            None => continue,
        };
        let template = registry.templates.get(&node.template_name);
        let is_selected = state.selected_nodes.contains(&node_id);
        let node_rect = node_screen_rect(node, template, canvas_rect, offset, zoom);

        draw_node(
            &painter,
            node,
            template,
            is_selected,
            node_rect,
            zoom,
            node_id,
        );
    }

    // Draw interactive widget sliders on nodes
    for &node_id in &node_ids {
        let node = match graph.nodes.get(&node_id) {
            Some(n) => n,
            None => continue,
        };
        if node.widget_decls.is_empty() { continue; }
        let template = registry.templates.get(&node.template_name);
        let node_rect = node_screen_rect(node, template, canvas_rect, offset, zoom);
        let preview_h = node_preview_height(node);
        if preview_h <= 0.0 { continue; }

        let preview_y = node_rect.bottom() - preview_h * zoom;
        let mut wy = preview_y + 4.0 * zoom;

        for wdecl in &node.widget_decls {
            if wdecl.widget_type != "slider" { continue; }
            let min = wdecl.params.first().copied().unwrap_or(0.0);
            let max = wdecl.params.get(1).copied().unwrap_or(100.0);
            let current = node.widget_values.get(&wdecl.name)
                .map(|v| v.as_f64())
                .unwrap_or(min);

            let slider_rect = Rect::from_min_size(
                pos2(node_rect.left() + 8.0 * zoom, wy + 2.0 * zoom),
                vec2(node_rect.width() - 16.0 * zoom, (WIDGET_ROW_HEIGHT - 4.0) * zoom),
            );

            // Draw slider track + fill
            let track_h = 4.0 * zoom;
            let track_rect = Rect::from_min_size(
                pos2(slider_rect.left(), slider_rect.center().y - track_h * 0.5),
                vec2(slider_rect.width(), track_h),
            );
            painter.rect_filled(track_rect, CornerRadius::same(2), Color32::from_rgb(0xdd, 0xdd, 0xdd));

            let frac = ((current - min) / (max - min).max(1e-10)).clamp(0.0, 1.0) as f32;
            let fill_rect = Rect::from_min_size(
                track_rect.left_top(),
                vec2(track_rect.width() * frac, track_h),
            );
            painter.rect_filled(fill_rect, CornerRadius::same(2), ACCENT);

            // Knob
            let knob_x = slider_rect.left() + slider_rect.width() * frac;
            painter.circle_filled(
                pos2(knob_x, slider_rect.center().y),
                5.0 * zoom,
                Color32::WHITE,
            );
            painter.circle_stroke(
                pos2(knob_x, slider_rect.center().y),
                5.0 * zoom,
                Stroke::new(1.0 * zoom, ACCENT),
            );

            // Handle drag on slider
            let slider_sense_rect = slider_rect.expand(2.0 * zoom);
            let slider_id = egui::Id::new(("node_slider", node_id, &wdecl.name));
            let slider_resp = ui.interact(slider_sense_rect, slider_id, Sense::drag());
            if slider_resp.dragged() {
                if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                    let local_x = (pos.x - slider_rect.left()) / slider_rect.width();
                    let new_val = min + (max - min) * local_x.clamp(0.0, 1.0) as f64;
                    response.widget_updates.push((node_id, wdecl.name.clone(), Value::F64(new_val)));
                }
            }

            wy += WIDGET_ROW_HEIGHT * zoom;
        }
    }

    // Click to select (no drag required)
    if canvas_response.clicked() {
        if let Some(cursor) = pointer_pos {
            let mut clicked_node = None;
            for &node_id in node_ids.iter().rev() {
                let node = &graph.nodes[&node_id];
                let template = registry.templates.get(&node.template_name);
                let rect = node_screen_rect(node, template, canvas_rect, offset, zoom);
                if rect.contains(cursor) {
                    clicked_node = Some(node_id);
                    break;
                }
            }

            if let Some(node_id) = clicked_node {
                if !ui.input(|i| i.modifiers.shift) {
                    state.selected_nodes = vec![node_id];
                } else if state.selected_nodes.contains(&node_id) {
                    state.selected_nodes.retain(|&id| id != node_id);
                } else {
                    state.selected_nodes.push(node_id);
                }
                response.node_selected = Some(node_id);
            } else if !ui.input(|i| i.modifiers.shift) {
                state.selected_nodes.clear();
                response.node_selected = None;
            }
        }
    }

    // Primary button drag interactions
    if canvas_response.drag_started_by(egui::PointerButton::Primary)
        && !ui.input(|i| i.key_down(egui::Key::Space))
        && !ui.input(|i| i.modifiers.ctrl)
    {
        if let Some(cursor) = pointer_pos {
            let mut clicked_node = None;
            for &node_id in node_ids.iter().rev() {
                let node = &graph.nodes[&node_id];
                let template = registry.templates.get(&node.template_name);
                let rect = node_screen_rect(node, template, canvas_rect, offset, zoom);
                if rect.contains(cursor) {
                    clicked_node = Some(node_id);
                    break;
                }
            }

            if let Some(node_id) = clicked_node {
                if !ui.input(|i| i.modifiers.shift) {
                    if !state.selected_nodes.contains(&node_id) {
                        state.selected_nodes = vec![node_id];
                    }
                } else if state.selected_nodes.contains(&node_id) {
                    state.selected_nodes.retain(|&id| id != node_id);
                } else {
                    state.selected_nodes.push(node_id);
                }
                let node_pos = graph.nodes[&node_id].pos;
                let screen_pos = pos2(
                    node_pos[0] * zoom + offset.x + canvas_rect.left(),
                    node_pos[1] * zoom + offset.y + canvas_rect.top(),
                );
                state.drag = DragKind::Node(node_id, cursor - screen_pos);
                response.node_selected = Some(node_id);
            } else {
                if !ui.input(|i| i.modifiers.shift) {
                    state.selected_nodes.clear();
                    response.node_selected = None;
                }
                state.drag = DragKind::BoxSelect(cursor);
            }
        }
    }

    // Handle ongoing drag
    if canvas_response.dragged_by(egui::PointerButton::Primary) {
        match &state.drag {
            DragKind::Node(node_id, grab_offset) => {
                if let Some(cursor) = pointer_pos {
                    let node_id = *node_id;
                    let go = *grab_offset;
                    let new_screen = cursor - go;
                    let new_x = (new_screen.x - canvas_rect.left() - offset.x) / zoom;
                    let new_y = (new_screen.y - canvas_rect.top() - offset.y) / zoom;

                    if let Some(node) = graph.nodes.get(&node_id) {
                        let dx = new_x - node.pos[0];
                        let dy = new_y - node.pos[1];

                        let selected = state.selected_nodes.clone();
                        for &sid in &selected {
                            if let Some(n) = graph.nodes.get_mut(&sid) {
                                n.pos[0] += dx;
                                n.pos[1] += dy;
                            }
                        }
                    }
                }
            }
            DragKind::BoxSelect(start) => {
                if let Some(cursor) = pointer_pos {
                    let select_rect = Rect::from_two_pos(*start, cursor);
                    painter.rect(
                        select_rect,
                        cr(1.0),
                        Color32::from_rgba_premultiplied(0x00, 0xd4, 0xff, 0x15),
                        Stroke::new(1.0, ACCENT.linear_multiply(0.5)),
                        StrokeKind::Outside,
                    );

                    let mut selected = Vec::new();
                    for (&node_id, node) in &graph.nodes {
                        let template = registry.templates.get(&node.template_name);
                        let rect = node_screen_rect(node, template, canvas_rect, offset, zoom);
                        if select_rect.intersects(rect) {
                            selected.push(node_id);
                        }
                    }
                    state.selected_nodes = selected;
                }
            }
            _ => {}
        }
    }

    // Handle drag release
    if canvas_response.drag_stopped_by(egui::PointerButton::Primary) {
        state.drag = DragKind::None;
    }

    // Right-click context menu
    if canvas_response.secondary_clicked() {
        if let Some(cursor) = pointer_pos {
            state.context_menu_pos = Some(cursor);
            state.show_context_menu = true;
        }
    }

    // Delete key — only when no text field is focused
    if ui.input(|i| i.key_pressed(egui::Key::Delete))
        && !state.selected_nodes.is_empty()
        && !ui.ctx().wants_keyboard_input()
    {
        response.delete_nodes = state.selected_nodes.clone();
        state.selected_nodes.clear();
    }

    // Draw minimap
    draw_minimap(&painter, canvas_rect, graph, registry, offset, zoom);

    response
}

fn draw_grid(painter: &Painter, rect: Rect, offset: Vec2, zoom: f32) {
    let grid_size = 40.0 * zoom;
    if grid_size < 5.0 {
        return;
    }

    let color = Color32::from_rgba_premultiplied(0x1e, 0x2d, 0x3d, 0x40);
    let start_x = rect.left() + (offset.x % grid_size);
    let start_y = rect.top() + (offset.y % grid_size);

    let mut x = start_x;
    while x < rect.right() {
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(0.5, color),
        );
        x += grid_size;
    }

    let mut y = start_y;
    while y < rect.bottom() {
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(0.5, color),
        );
        y += grid_size;
    }
}

fn node_width(template: Option<&NodeTemplate>, node: &Node) -> f32 {
    let base = if template.map_or(false, |t| t.builtin == Some(BuiltinKind::Script)) {
        SCRIPT_NODE_WIDTH
    } else {
        NODE_WIDTH
    };
    // Expand width if canvas blocks need more space
    let mut max_canvas_w = 0.0f32;
    for block in &node.render_blocks {
        if let crate::render::RenderBlock::Canvas { width, .. } = block {
            max_canvas_w = max_canvas_w.max(*width as f32 + CANVAS_PREVIEW_PADDING * 2.0);
        }
    }
    base.max(max_canvas_w)
}

const WIDGET_ROW_HEIGHT: f32 = 22.0;
const CANVAS_PREVIEW_PADDING: f32 = 4.0;

fn node_preview_height(node: &Node) -> f32 {
    let mut h = 0.0;
    h += node.widget_decls.len() as f32 * WIDGET_ROW_HEIGHT;
    for block in &node.render_blocks {
        if let crate::render::RenderBlock::Canvas { height, .. } = block {
            h += *height as f32 + CANVAS_PREVIEW_PADDING * 2.0;
        }
    }
    if h > 0.0 { h += 4.0; }
    h
}

fn node_height(template: Option<&NodeTemplate>, node: &Node) -> f32 {
    let ins = node.effective_inputs(template).len();
    let outs = node.effective_outputs(template).len();
    let port_count = ins.max(outs).max(1);
    let has_result = !node.output_values.is_empty()
        || node.error.is_some()
        || template.map_or(false, |t| t.builtin == Some(BuiltinKind::Const));

    HEADER_HEIGHT
        + port_count as f32 * PORT_ROW_HEIGHT
        + if has_result { RESULT_HEIGHT } else { 0.0 }
        + node_preview_height(node)
        + 4.0
}

fn node_screen_rect(
    node: &Node,
    template: Option<&NodeTemplate>,
    canvas_rect: Rect,
    offset: Vec2,
    zoom: f32,
) -> Rect {
    let x = node.pos[0] * zoom + offset.x + canvas_rect.left();
    let y = node.pos[1] * zoom + offset.y + canvas_rect.top();
    let w = node_width(template, node) * zoom;
    let h = node_height(template, node) * zoom;
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

fn port_screen_pos(
    node: &Node,
    template: Option<&NodeTemplate>,
    port_name: &str,
    is_output: bool,
    canvas_rect: Rect,
    offset: Vec2,
    zoom: f32,
) -> Option<Pos2> {
    let template = template?;
    let ports = if is_output {
        node.effective_outputs(Some(template))
    } else {
        node.effective_inputs(Some(template))
    };
    let idx = ports.iter().position(|p| p.name == port_name)?;

    let x = node.pos[0] * zoom + offset.x + canvas_rect.left();
    let y = node.pos[1] * zoom + offset.y + canvas_rect.top();

    let nw = node_width(Some(template), node);
    let port_x = if is_output {
        x + nw * zoom
    } else {
        x
    };
    let port_y = y + (HEADER_HEIGHT + PORT_ROW_HEIGHT * (idx as f32 + 0.5)) * zoom;

    Some(pos2(port_x, port_y))
}

fn draw_wire(painter: &Painter, from: Pos2, to: Pos2, color: Color32, is_dragging: bool) {
    let dx = (to.x - from.x).abs() * 0.5;
    let cp1 = pos2(from.x + dx, from.y);
    let cp2 = pos2(to.x - dx, to.y);

    let points = bezier_points(from, cp1, cp2, to, 32);

    // Glow
    let glow_color = Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 0x26);
    let glow_width = if is_dragging { 8.0 } else { 6.0 };
    for i in 0..points.len() - 1 {
        painter.line_segment(
            [points[i], points[i + 1]],
            Stroke::new(glow_width, glow_color),
        );
    }

    // Main wire
    let width = if is_dragging { 2.0 } else { 1.5 };
    for i in 0..points.len() - 1 {
        painter.line_segment([points[i], points[i + 1]], Stroke::new(width, color));
    }
}

fn bezier_points(p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2, steps: usize) -> Vec<Pos2> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let u = 1.0 - t;
            let x = u * u * u * p0.x
                + 3.0 * u * u * t * p1.x
                + 3.0 * u * t * t * p2.x
                + t * t * t * p3.x;
            let y = u * u * u * p0.y
                + 3.0 * u * u * t * p1.y
                + 3.0 * u * t * t * p2.y
                + t * t * t * p3.y;
            pos2(x, y)
        })
        .collect()
}

fn draw_node(
    painter: &Painter,
    node: &Node,
    template: Option<&NodeTemplate>,
    is_selected: bool,
    rect: Rect,
    zoom: f32,
    _node_id: NodeId,
) {
    let accent = if node.phantom {
        Color32::from_rgb(0x88, 0x66, 0xcc) // purple for phantom/remote nodes
    } else {
        template
            .map(|t| node_accent_color(&t.name))
            .unwrap_or(COLOR_CUSTOM)
    };

    let rounding = cr(6.0 * zoom);

    // Shadow
    painter.rect_filled(
        rect.translate(vec2(2.0, 2.0)),
        rounding,
        Color32::from_black_alpha(60),
    );

    // Background (phantom nodes are semi-transparent)
    let bg = if node.phantom {
        Color32::from_rgba_premultiplied(0x2a, 0x2a, 0x2e, 0xcc)
    } else {
        NODE_BG
    };
    painter.rect_filled(rect, rounding, bg);

    // Border
    let border_color = if node.error.is_some() {
        Color32::from_rgb(0xff, 0x33, 0x33)
    } else if is_selected {
        NODE_SELECTED
    } else {
        NODE_BORDER
    };
    let border_width = if is_selected { 1.5 } else { 1.0 };
    painter.rect_stroke(
        rect,
        rounding,
        Stroke::new(border_width * zoom, border_color),
        StrokeKind::Inside,
    );

    // Selection glow
    if is_selected {
        let glow_color = Color32::from_rgba_premultiplied(0x00, 0xd4, 0xff, 0x20);
        painter.rect_stroke(
            rect.expand(2.0 * zoom),
            cr(8.0 * zoom),
            Stroke::new(2.0 * zoom, glow_color),
            StrokeKind::Outside,
        );
    }

    // Accent strip (left border)
    let strip_rect = Rect::from_min_size(
        rect.left_top(),
        vec2(ACCENT_STRIP_WIDTH * zoom, HEADER_HEIGHT * zoom),
    );
    let strip_rounding = {
        let r = (6.0 * zoom).round().max(0.0) as u8;
        CornerRadius {
            nw: r,
            ne: 0,
            sw: 0,
            se: 0,
        }
    };
    painter.rect_filled(strip_rect, strip_rounding, accent);

    // Header
    let header_rect =
        Rect::from_min_size(rect.left_top(), vec2(rect.width(), HEADER_HEIGHT * zoom));
    let font = FontId::proportional(13.0 * zoom);
    let label_text = if node.phantom {
        format!("\u{1F4E1} {}", node.label)
    } else {
        node.label.clone()
    };
    painter.text(
        pos2(
            rect.left() + (ACCENT_STRIP_WIDTH + 8.0) * zoom,
            header_rect.center().y,
        ),
        egui::Align2::LEFT_CENTER,
        &label_text,
        font.clone(),
        if node.phantom { Color32::from_rgb(0xbb, 0x99, 0xff) } else { TEXT },
    );

    // WASM badge
    if template.map_or(false, |t| t.builtin.is_none()) {
        let badge_font = FontId::proportional(9.0 * zoom);
        painter.text(
            pos2(rect.right() - 8.0 * zoom, header_rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "WASM",
            badge_font,
            TEXT_DIM,
        );
    }

    // Header separator
    let sep_y = rect.top() + HEADER_HEIGHT * zoom;
    painter.line_segment(
        [
            pos2(rect.left() + 4.0, sep_y),
            pos2(rect.right() - 4.0, sep_y),
        ],
        Stroke::new(0.5, NODE_BORDER),
    );

    // Ports
    if let Some(template) = template {
        let port_font = FontId::proportional(11.0 * zoom);

        let eff_inputs = node.effective_inputs(Some(template));
        let eff_outputs = node.effective_outputs(Some(template));

        for (i, port) in eff_inputs.iter().enumerate() {
            let py = rect.top() + (HEADER_HEIGHT + PORT_ROW_HEIGHT * (i as f32 + 0.5)) * zoom;
            let px = rect.left();

            painter.circle(
                pos2(px, py),
                PORT_RADIUS * zoom,
                Color32::TRANSPARENT,
                Stroke::new(1.5 * zoom, port.port_type.color()),
            );

            painter.text(
                pos2(px + (PORT_RADIUS + 6.0) * zoom, py),
                egui::Align2::LEFT_CENTER,
                &port.name,
                port_font.clone(),
                TEXT_DIM,
            );

        }

        for (i, port) in eff_outputs.iter().enumerate() {
            let py = rect.top() + (HEADER_HEIGHT + PORT_ROW_HEIGHT * (i as f32 + 0.5)) * zoom;
            let px = rect.right();

            painter.circle_filled(pos2(px, py), PORT_RADIUS * zoom, port.port_type.color());

            painter.text(
                pos2(px - (PORT_RADIUS + 6.0) * zoom, py),
                egui::Align2::RIGHT_CENTER,
                &port.name,
                port_font.clone(),
                TEXT_DIM,
            );

        }

        // Result display
        let port_count = eff_inputs.len().max(eff_outputs.len()).max(1);
        let result_y =
            rect.top() + (HEADER_HEIGHT + PORT_ROW_HEIGHT * port_count as f32 + 2.0) * zoom;

        let display_text = if let Some(err) = &node.error {
            Some((
                err.chars().take(30).collect::<String>(),
                Color32::from_rgb(0xff, 0x44, 0x44),
            ))
        } else if template.builtin == Some(BuiltinKind::Const) {
            let val = node
                .input_values
                .get("value")
                .map(|v| v.display())
                .unwrap_or_else(|| "0.000000".to_string());
            Some((val, accent))
        } else if let Some(val) = node.output_values.values().next() {
            Some((val.display(), accent))
        } else {
            None
        };

        if let Some((text, color)) = display_text {
            painter.line_segment(
                [
                    pos2(rect.left() + 4.0, result_y - 2.0 * zoom),
                    pos2(rect.right() - 4.0, result_y - 2.0 * zoom),
                ],
                Stroke::new(0.5, NODE_BORDER),
            );

            let result_font = FontId::monospace(11.0 * zoom);
            painter.text(
                pos2(rect.left() + 10.0 * zoom, result_y + 8.0 * zoom),
                egui::Align2::LEFT_CENTER,
                format!("[ {} ]", text),
                result_font,
                color,
            );
        }

        // Draw preview: canvas blocks
        let preview_h = node_preview_height(node);
        if preview_h > 0.0 {
            let preview_y = rect.bottom() - preview_h * zoom;
            painter.line_segment(
                [
                    pos2(rect.left() + 4.0, preview_y),
                    pos2(rect.right() - 4.0, preview_y),
                ],
                Stroke::new(0.5, NODE_BORDER),
            );
            let mut cy = preview_y + 4.0 * zoom;
            // Widget decl labels (actual sliders drawn via ui later)
            for wdecl in &node.widget_decls {
                let font = FontId::proportional(10.0 * zoom);
                painter.text(
                    pos2(rect.left() + 8.0 * zoom, cy + WIDGET_ROW_HEIGHT * zoom * 0.5),
                    egui::Align2::LEFT_CENTER,
                    &wdecl.name,
                    font,
                    TEXT_DIM,
                );
                cy += WIDGET_ROW_HEIGHT * zoom;
            }
            // Canvas blocks
            for block in &node.render_blocks {
                if let crate::render::RenderBlock::Canvas { width, height, commands } = block {
                    let cw = *width as f32;
                    let ch = *height as f32;
                    let scale = zoom.min((rect.width() - CANVAS_PREVIEW_PADDING * 2.0 * zoom) / cw);
                    let origin = pos2(
                        rect.left() + CANVAS_PREVIEW_PADDING * zoom,
                        cy + CANVAS_PREVIEW_PADDING * zoom,
                    );
                    paint_draw_cmds(painter, commands, origin, scale);
                    cy += (ch + CANVAS_PREVIEW_PADDING * 2.0) * zoom;
                }
            }
        }

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

fn paint_draw_cmds(painter: &Painter, commands: &[DrawCmd], origin: Pos2, scale: f32) {
    for cmd in commands {
        match cmd {
            DrawCmd::Line { x1, y1, x2, y2, color, width } => {
                painter.line_segment(
                    [
                        origin + vec2(*x1 as f32 * scale, *y1 as f32 * scale),
                        origin + vec2(*x2 as f32 * scale, *y2 as f32 * scale),
                    ],
                    Stroke::new(*width as f32 * scale, parse_hex_color(color)),
                );
            }
            DrawCmd::Rect { x, y, w, h, fill } => {
                let r = Rect::from_min_size(
                    origin + vec2(*x as f32 * scale, *y as f32 * scale),
                    vec2(*w as f32 * scale, *h as f32 * scale),
                );
                painter.rect_filled(r, CornerRadius::ZERO, parse_hex_color(fill));
            }
            DrawCmd::Circle { x, y, r, fill } => {
                painter.circle_filled(
                    origin + vec2(*x as f32 * scale, *y as f32 * scale),
                    *r as f32 * scale,
                    parse_hex_color(fill),
                );
            }
            DrawCmd::Polyline { points, color, width } => {
                if points.len() >= 2 {
                    let stroke = Stroke::new(*width as f32 * scale, parse_hex_color(color));
                    let pts: Vec<Pos2> = points.iter()
                        .map(|p| origin + vec2(p[0] as f32 * scale, p[1] as f32 * scale))
                        .collect();
                    for pair in pts.windows(2) {
                        painter.line_segment([pair[0], pair[1]], stroke);
                    }
                }
            }
            DrawCmd::Text { x, y, text, color, size } => {
                let font = FontId::proportional(*size as f32 * scale);
                painter.text(
                    origin + vec2(*x as f32 * scale, *y as f32 * scale),
                    egui::Align2::LEFT_TOP,
                    text,
                    font,
                    parse_hex_color(color),
                );
            }
        }
    }
}

fn draw_minimap(
    painter: &Painter,
    canvas_rect: Rect,
    graph: &Graph,
    registry: &NodeRegistry,
    offset: Vec2,
    zoom: f32,
) {
    if graph.nodes.is_empty() {
        return;
    }

    let minimap_w = 160.0;
    let minimap_h = 100.0;
    let margin = 10.0;

    let minimap_rect = Rect::from_min_size(
        pos2(
            canvas_rect.right() - minimap_w - margin,
            canvas_rect.bottom() - minimap_h - margin,
        ),
        vec2(minimap_w, minimap_h),
    );

    painter.rect_filled(
        minimap_rect,
        cr(4.0),
        Color32::from_rgba_premultiplied(0x08, 0x0b, 0x10, 0xcc),
    );
    painter.rect_stroke(
        minimap_rect,
        cr(4.0),
        Stroke::new(1.0, NODE_BORDER),
        StrokeKind::Inside,
    );

    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;

    for node in graph.nodes.values() {
        min_x = min_x.min(node.pos[0]);
        min_y = min_y.min(node.pos[1]);
        max_x = max_x.max(node.pos[0] + NODE_WIDTH);
        max_y = max_y.max(node.pos[1] + 100.0);
    }

    let world_w = (max_x - min_x).max(200.0) + 100.0;
    let world_h = (max_y - min_y).max(200.0) + 100.0;
    let scale = (minimap_w / world_w).min(minimap_h / world_h) * 0.8;

    let center_x = minimap_rect.center().x;
    let center_y = minimap_rect.center().y;

    for node in graph.nodes.values() {
        let template = registry.templates.get(&node.template_name);
        let color = template
            .map(|t| node_accent_color(&t.name))
            .unwrap_or(COLOR_CUSTOM);

        let nx = center_x + (node.pos[0] - (min_x + max_x) / 2.0) * scale;
        let ny = center_y + (node.pos[1] - (min_y + max_y) / 2.0) * scale;
        let nw = NODE_WIDTH * scale;
        let nh = 10.0 * scale;

        painter.rect_filled(
            Rect::from_min_size(pos2(nx, ny), vec2(nw.max(3.0), nh.max(2.0))),
            cr(1.0),
            color.linear_multiply(0.7),
        );
    }

    let vp_x = center_x + (-offset.x / zoom - (min_x + max_x) / 2.0) * scale;
    let vp_y = center_y + (-offset.y / zoom - (min_y + max_y) / 2.0) * scale;
    let vp_w = canvas_rect.width() / zoom * scale;
    let vp_h = canvas_rect.height() / zoom * scale;

    painter.rect_stroke(
        Rect::from_min_size(pos2(vp_x, vp_y), vec2(vp_w, vp_h)),
        cr(1.0),
        Stroke::new(
            1.0,
            Color32::from_rgba_premultiplied(0xff, 0xff, 0xff, 0x80),
        ),
        StrokeKind::Outside,
    );
}

#[derive(Default)]
pub struct CanvasResponse {
    pub node_selected: Option<NodeId>,
    pub delete_nodes: Vec<NodeId>,
    pub widget_updates: Vec<(NodeId, String, Value)>,
}
