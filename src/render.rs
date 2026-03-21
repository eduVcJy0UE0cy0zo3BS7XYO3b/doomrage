use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoreAction {
    Set { key: String, value: String },
    Append { key: String, value: String },
    Delete { key: String },
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
