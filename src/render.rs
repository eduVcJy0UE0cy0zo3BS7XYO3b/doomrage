use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DrawCmd {
    Line { x1: f64, y1: f64, x2: f64, y2: f64, color: String, width: f64 },
    Rect { x: f64, y: f64, w: f64, h: f64, fill: String },
    Circle { x: f64, y: f64, r: f64, fill: String },
    Polyline { points: Vec<[f64; 2]>, color: String, width: f64 },
    Text { x: f64, y: f64, text: String, color: String, size: f64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RenderBlock {
    Text(String),
    Bold(String),
    Italic(String),
    Code(String),
    Link { url: String, label: String },
    Hr,
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Plot(PlotData),
    Group(Vec<RenderBlock>),
    // Interactive widgets
    Button { label: String, action: StoreAction },
    Checkbox { label: String, key: String },
    TextInput { key: String, placeholder: String },
    Slider { key: String, min: f64, max: f64 },
    EditableList { key: String },
    Canvas { width: f64, height: f64, commands: Vec<DrawCmd> },
    // Layout & composition
    Row(Vec<Vec<RenderBlock>>),
    Frame(Vec<RenderBlock>),
    NodeView { label: String },
    NodeBlocks { label: String },
    NodeWidgets { label: String },
    NodeWidget { label: String, widget_name: String },
    /// Generic event wrapper: children are rendered, events are dispatched on interaction
    Interactive {
        events: Vec<(String, Vec<String>)>,  // (event_type, message_parts)
        children: Vec<RenderBlock>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoreAction {
    Set { key: String, value: String },
    Append { key: String, value: String },
    Delete { key: String },
    /// Splice array at index: remove `delete_count` elements, insert `value` (if non-empty).
    /// (button "x" 'splice (list "todos" 2 1 ""))    → remove index 2
    /// (button "v" 'splice (list "todos" 2 1 "done")) → replace index 2 with "done"
    Splice { key: String, index: usize, delete_count: usize, value: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlotData {
    Line {
        y: Vec<f64>,
        title: Option<String>,
    },
    Scatter {
        x: Vec<f64>,
        y: Vec<f64>,
        title: Option<String>,
    },
    Bar {
        labels: Vec<String>,
        values: Vec<f64>,
        title: Option<String>,
    },
}
