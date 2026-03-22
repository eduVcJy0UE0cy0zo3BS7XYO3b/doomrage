use crate::render::{PlotData, RenderBlock};
use crate::theme::*;
use egui::{pos2, Color32, FontId, Painter, Pos2, Rect, Stroke, vec2};

pub struct RenderMetrics {
    pub total_height: f32,
}

/// Render blocks using the Painter at a given position. Returns total height used.
pub fn paint_render_blocks(
    painter: &Painter,
    blocks: &[RenderBlock],
    top_left: Pos2,
    max_width: f32,
    zoom: f32,
) -> RenderMetrics {
    let mut y = top_left.y;
    let x = top_left.x;
    let line_height = 14.0 * zoom;
    let padding = 4.0 * zoom;

    for block in blocks {
        match block {
            RenderBlock::Text(t) => {
                let font = FontId::proportional(11.0 * zoom);
                painter.text(
                    pos2(x + padding, y),
                    egui::Align2::LEFT_TOP,
                    t,
                    font,
                    TEXT,
                );
                y += line_height;
            }
            RenderBlock::Bold(t) => {
                let font = FontId::proportional(11.0 * zoom);
                painter.text(
                    pos2(x + padding, y),
                    egui::Align2::LEFT_TOP,
                    t,
                    font,
                    ACCENT,
                );
                y += line_height;
            }
            RenderBlock::Italic(t) => {
                let font = FontId::proportional(11.0 * zoom);
                painter.text(
                    pos2(x + padding, y),
                    egui::Align2::LEFT_TOP,
                    t,
                    font,
                    Color32::from_rgb(0x55, 0x55, 0x88),
                );
                y += line_height;
            }
            RenderBlock::Code(t) => {
                let font = FontId::monospace(10.0 * zoom);
                let text_rect = Rect::from_min_size(
                    pos2(x + padding, y),
                    vec2(max_width - padding * 2.0, line_height),
                );
                painter.rect_filled(
                    text_rect,
                    egui::CornerRadius::same(2),
                    Color32::from_rgb(0xf0, 0xf0, 0xec),
                );
                painter.text(
                    pos2(x + padding * 2.0, y + 1.0 * zoom),
                    egui::Align2::LEFT_TOP,
                    t,
                    font,
                    Color32::from_rgb(0x22, 0x8b, 0x22),
                );
                y += line_height + 2.0 * zoom;
            }
            RenderBlock::Link { label, .. } => {
                let font = FontId::proportional(11.0 * zoom);
                painter.text(
                    pos2(x + padding, y),
                    egui::Align2::LEFT_TOP,
                    label,
                    font,
                    Color32::from_rgb(0x00, 0x66, 0xcc),
                );
                // Underline
                let text_width = label.len() as f32 * 6.0 * zoom;
                painter.line_segment(
                    [
                        pos2(x + padding, y + line_height - 1.0),
                        pos2(x + padding + text_width, y + line_height - 1.0),
                    ],
                    Stroke::new(1.0, Color32::from_rgb(0x00, 0x66, 0xcc)),
                );
                y += line_height;
            }
            RenderBlock::Hr => {
                y += 2.0 * zoom;
                painter.line_segment(
                    [
                        pos2(x + padding, y),
                        pos2(x + max_width - padding, y),
                    ],
                    Stroke::new(1.0, NODE_BORDER),
                );
                y += 4.0 * zoom;
            }
            RenderBlock::Table { headers, rows } => {
                let font = FontId::monospace(10.0 * zoom);
                let col_width = (max_width - padding * 2.0)
                    / headers.len().max(1) as f32;

                // Headers
                for (i, h) in headers.iter().enumerate() {
                    painter.text(
                        pos2(x + padding + col_width * i as f32, y),
                        egui::Align2::LEFT_TOP,
                        h,
                        font.clone(),
                        ACCENT,
                    );
                }
                y += line_height;

                // Separator
                painter.line_segment(
                    [
                        pos2(x + padding, y - 1.0),
                        pos2(x + max_width - padding, y - 1.0),
                    ],
                    Stroke::new(0.5, NODE_BORDER),
                );

                // Rows
                for row in rows {
                    for (i, cell) in row.iter().enumerate() {
                        painter.text(
                            pos2(x + padding + col_width * i as f32, y),
                            egui::Align2::LEFT_TOP,
                            cell,
                            font.clone(),
                            TEXT_DIM,
                        );
                    }
                    y += line_height;
                }
                y += 2.0 * zoom;
            }
            RenderBlock::Plot(plot_data) => {
                let plot_height = 60.0 * zoom;
                let plot_width = max_width - padding * 2.0;
                let plot_rect = Rect::from_min_size(
                    pos2(x + padding, y),
                    vec2(plot_width, plot_height),
                );

                // Background
                painter.rect_filled(
                    plot_rect,
                    egui::CornerRadius::same(2),
                    Color32::from_rgb(0xf0, 0xf0, 0xec),
                );

                match plot_data {
                    PlotData::Line { y: data, title } => {
                        if !data.is_empty() {
                            let min_v = data.iter().copied().fold(f64::INFINITY, f64::min);
                            let max_v = data.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                            let range = (max_v - min_v).max(0.001);

                            let points: Vec<Pos2> = data
                                .iter()
                                .enumerate()
                                .map(|(i, &v)| {
                                    let px = plot_rect.left()
                                        + (i as f32 / (data.len() - 1).max(1) as f32)
                                            * plot_width;
                                    let py = plot_rect.bottom()
                                        - ((v - min_v) / range) as f32 * plot_height;
                                    pos2(px, py)
                                })
                                .collect();

                            for w in points.windows(2) {
                                painter.line_segment(
                                    [w[0], w[1]],
                                    Stroke::new(1.5 * zoom, ACCENT),
                                );
                            }
                        }

                        if let Some(title) = title {
                            let font = FontId::proportional(9.0 * zoom);
                            painter.text(
                                pos2(plot_rect.left() + 2.0, plot_rect.top() + 1.0),
                                egui::Align2::LEFT_TOP,
                                title,
                                font,
                                TEXT_DIM,
                            );
                        }
                    }
                    _ => {
                        // Placeholder for other plot types
                        let font = FontId::proportional(10.0 * zoom);
                        painter.text(
                            plot_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "[plot]",
                            font,
                            TEXT_DIM,
                        );
                    }
                }

                y += plot_height + 4.0 * zoom;
            }
            RenderBlock::Group(blocks) => {
                let sub = paint_render_blocks(painter, blocks, pos2(x, y), max_width, zoom);
                y += sub.total_height;
            }
            // Interactive widgets are rendered in the inspector panel, not on canvas
            RenderBlock::Button { .. }
            | RenderBlock::Checkbox { .. }
            | RenderBlock::TextInput { .. }
            | RenderBlock::Slider { .. }
            | RenderBlock::EditableList { .. } => {
                y += line_height;
            }
            RenderBlock::Canvas { height, .. } => {
                // Canvas blocks are rendered in the inspector panel
                y += *height as f32 * zoom;
            }
            // Layout/composition blocks: skip on canvas preview
            RenderBlock::Row(_)
            | RenderBlock::Frame(_)
            | RenderBlock::NodeView { .. }
            | RenderBlock::NodeBlocks { .. }
            | RenderBlock::NodeWidgets { .. }
            | RenderBlock::NodeWidget { .. } => {
                y += line_height;
            }
        }
    }

    RenderMetrics {
        total_height: y - top_left.y,
    }
}
