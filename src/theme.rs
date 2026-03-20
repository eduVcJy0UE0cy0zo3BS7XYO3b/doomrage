use egui::{Color32, Visuals, Style, Stroke, Shadow};

pub const BG: Color32 = Color32::from_rgb(0xf5, 0xf5, 0xf0);
pub const PANEL: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const NODE_BG: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const NODE_BORDER: Color32 = Color32::from_rgb(0xd0, 0xd0, 0xd0);
pub const NODE_SELECTED: Color32 = Color32::from_rgb(0x00, 0x7a, 0xcc);
pub const WIRE_DEFAULT: Color32 = Color32::from_rgb(0x90, 0xa0, 0xb0);
pub const TEXT: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
pub const ACCENT: Color32 = Color32::from_rgb(0x00, 0x7a, 0xcc);

pub const COLOR_CONST: Color32 = Color32::from_rgb(0x00, 0x7a, 0xcc);
pub const COLOR_MATH: Color32 = Color32::from_rgb(0x22, 0x8b, 0x22);
pub const COLOR_TRIG: Color32 = Color32::from_rgb(0x99, 0x33, 0xcc);
pub const COLOR_STRING: Color32 = Color32::from_rgb(0xcc, 0x77, 0x00);
pub const COLOR_OUTPUT: Color32 = Color32::from_rgb(0xdd, 0x44, 0x00);
pub const COLOR_CUSTOM: Color32 = Color32::from_rgb(0x66, 0x66, 0x66);

pub fn node_accent_color(template_name: &str) -> Color32 {
    match template_name {
        "Const" => COLOR_CONST,
        "Output" => COLOR_OUTPUT,
        "Script" => COLOR_TRIG,
        "add" | "sub" | "mul" | "div" | "abs" | "clamp" | "lerp" => COLOR_MATH,
        "sqrt" | "sin" | "cos" | "tan" => COLOR_TRIG,
        name if name.contains("string") || name.contains("str") => COLOR_STRING,
        _ => COLOR_CUSTOM,
    }
}

pub fn apply_theme(ctx: &egui::Context) {
    let mut style = Style::default();

    let mut visuals = Visuals::light();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(0xf0, 0xf0, 0xec);
    visuals.faint_bg_color = Color32::from_rgb(0xf8, 0xf8, 0xf5);

    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(0.5, NODE_BORDER);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(4);

    visuals.widgets.inactive.bg_fill = Color32::from_rgb(0xf0, 0xf0, 0xec);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.bg_stroke = Stroke::new(0.5, NODE_BORDER);

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0xe8, 0xf0, 0xf8);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);

    visuals.widgets.active.bg_fill = Color32::from_rgb(0xd8, 0xe8, 0xf4);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);

    visuals.selection.bg_fill = Color32::from_rgba_premultiplied(0x00, 0x7a, 0xcc, 0x30);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow {
        offset: [0, 2],
        blur: 8,
        spread: 0,
        color: Color32::from_black_alpha(30),
    };

    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);

    ctx.set_style(style);
}
