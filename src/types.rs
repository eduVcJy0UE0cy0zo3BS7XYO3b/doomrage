use crate::scheme_engine::extract_imports;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PortType {
    F64,
    F32,
    I64,
    I32,
    Bool,
    Str,
}

impl PortType {
    pub fn color(&self) -> egui::Color32 {
        match self {
            PortType::F64 => egui::Color32::from_rgb(0x00, 0xdd, 0x66),
            PortType::F32 => egui::Color32::from_rgb(0x00, 0xbb, 0x55),
            PortType::I64 => egui::Color32::from_rgb(0x00, 0xaa, 0xff),
            PortType::I32 => egui::Color32::from_rgb(0x00, 0x88, 0xdd),
            PortType::Bool => egui::Color32::from_rgb(0xcc, 0x44, 0xff),
            PortType::Str => egui::Color32::from_rgb(0xff, 0xaa, 0x00),
        }
    }

    pub fn default_value(&self) -> Value {
        match self {
            PortType::F64 => Value::F64(0.0),
            PortType::F32 => Value::F32(0.0),
            PortType::I64 => Value::I64(0),
            PortType::I32 => Value::I32(0),
            PortType::Bool => Value::Bool(false),
            PortType::Str => Value::Str(String::new()),
        }
    }

    pub fn from_str(s: &str) -> Option<PortType> {
        match s {
            "f64" => Some(PortType::F64),
            "f32" => Some(PortType::F32),
            "i64" => Some(PortType::I64),
            "i32" => Some(PortType::I32),
            "bool" => Some(PortType::Bool),
            "string" | "str" => Some(PortType::Str),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            PortType::F64 => "f64",
            PortType::F32 => "f32",
            PortType::I64 => "i64",
            PortType::I32 => "i32",
            PortType::Bool => "bool",
            PortType::Str => "string",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Value {
    F64(f64),
    F32(f32),
    I64(i64),
    I32(i32),
    Bool(bool),
    Str(String),
}

impl Value {
    pub fn port_type(&self) -> PortType {
        match self {
            Value::F64(_) => PortType::F64,
            Value::F32(_) => PortType::F32,
            Value::I64(_) => PortType::I64,
            Value::I32(_) => PortType::I32,
            Value::Bool(_) => PortType::Bool,
            Value::Str(_) => PortType::Str,
        }
    }

    pub fn as_f64(&self) -> f64 {
        match self {
            Value::F64(v) => *v,
            Value::F32(v) => *v as f64,
            Value::I64(v) => *v as f64,
            Value::I32(v) => *v as f64,
            Value::Bool(v) => if *v { 1.0 } else { 0.0 },
            Value::Str(_) => f64::NAN,
        }
    }

    pub fn to_scheme_literal(&self) -> String {
        match self {
            Value::F64(f) => format!("{}", f),
            Value::F32(f) => format!("{}", f),
            Value::I64(i) => format!("{}", i),
            Value::I32(i) => format!("{}", i),
            Value::Bool(b) => if *b { "#t" } else { "#f" }.to_string(),
            Value::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        }
    }

    pub fn display(&self) -> String {
        match self {
            Value::F64(v) => format!("{v:.6}"),
            Value::F32(v) => format!("{v:.4}"),
            Value::I64(v) => format!("{v}"),
            Value::I32(v) => format!("{v}"),
            Value::Bool(v) => format!("{v}"),
            Value::Str(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDef {
    pub name: String,
    pub port_type: PortType,
}

#[derive(Debug, Clone)]
pub struct NodeTemplate {
    pub name: String,
    pub category: String,
    pub path: Option<PathBuf>,
    pub inputs: Vec<PortDef>,
    pub outputs: Vec<PortDef>,
    pub wasm_bytes: Option<Vec<u8>>,
    pub builtin: Option<BuiltinKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinKind {
    Const,
    Output,
    Script,
}

pub type NodeId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub template_name: String,
    pub label: String,
    pub pos: [f32; 2],
    pub input_values: HashMap<String, Value>,
    pub output_values: HashMap<String, Value>,
    #[serde(default)]
    pub script_code: String,
    /// Dynamic ports parsed from script_code (for Script nodes)
    #[serde(default)]
    pub script_inputs: Vec<PortDef>,
    #[serde(default)]
    pub script_outputs: Vec<PortDef>,
    #[serde(default)]
    pub widget_decls: Vec<crate::bridge::WidgetDecl>,
    #[serde(default)]
    pub widget_values: HashMap<String, Value>,
    #[serde(skip)]
    pub error: Option<String>,
    #[serde(skip)]
    pub last_exec_us: Option<u64>,
    #[serde(skip)]
    pub render_blocks: Vec<crate::render::RenderBlock>,
}

impl Node {
    /// Get effective input ports (script_inputs for Script nodes, template inputs otherwise)
    pub fn effective_inputs<'a>(&'a self, template: Option<&'a NodeTemplate>) -> &'a [PortDef] {
        if !self.script_inputs.is_empty() {
            &self.script_inputs
        } else {
            template.map(|t| t.inputs.as_slice()).unwrap_or(&[])
        }
    }

    pub fn effective_outputs<'a>(&'a self, template: Option<&'a NodeTemplate>) -> &'a [PortDef] {
        if !self.script_outputs.is_empty() {
            &self.script_outputs
        } else {
            template.map(|t| t.outputs.as_slice()).unwrap_or(&[])
        }
    }
}

/// Dependency edge: port-level (Some port names) or node-level (None).
#[derive(Debug, Clone)]
pub struct Connection {
    pub from_node: NodeId,
    pub to_node: NodeId,
    pub from_port: Option<String>,
    pub to_port: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: HashMap<NodeId, Node>,
    pub next_node_id: NodeId,
    pub viewport_offset: [f32; 2],
    pub viewport_zoom: f32,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            next_node_id: 1,
            viewport_offset: [0.0, 0.0],
            viewport_zoom: 1.0,
        }
    }

    pub fn add_node(&mut self, template: &NodeTemplate, pos: [f32; 2]) -> NodeId {
        let id = self.next_node_id;
        self.next_node_id += 1;

        let mut input_values = HashMap::new();
        for port in &template.inputs {
            input_values.insert(port.name.clone(), port.port_type.default_value());
        }

        let script_code = if template.builtin == Some(BuiltinKind::Script) {
            "(define x (input 'x 'f64))\n(define y (input 'y 'f64))\n(define result (output 'result 'f64))\n(set! result (+ x y))\n\n# Result\n\nThe sum is @result.\n\n| input | value |\n|-------|-------|\n| x     | @x    |\n| y     | @y    |".to_string()
        } else {
            String::new()
        };

        let node = Node {
            id,
            template_name: template.name.clone(),
            label: template.name.clone(),
            pos,
            input_values,
            output_values: HashMap::new(),
            script_code,
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
        };
        self.nodes.insert(id, node);
        id
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.nodes.remove(&id);
    }

    /// Find node ID by sanitized label (spaces → hyphens).
    pub fn find_node_by_import_label(&self, import_label: &str) -> Option<NodeId> {
        self.nodes.iter().find_map(|(&id, n)| {
            if n.label.replace(' ', "-") == import_label {
                Some(id)
            } else {
                None
            }
        })
    }

    /// Build import-based dependency edges: (source_id, target_id) for each import.
    fn import_edges(&self) -> Vec<(NodeId, NodeId)> {
        let mut edges = Vec::new();
        for (&target_id, target_node) in &self.nodes {
            for import_label in extract_imports(&target_node.script_code) {
                if let Some(source_id) = self.find_node_by_import_label(&import_label) {
                    edges.push((source_id, target_id));
                }
            }
        }
        edges
    }

    /// Derive connections on-the-fly from imports in script code.
    /// Only port-level arrows (matching export→import port names).
    /// No arrow if no ports match (UI-only dependency stays in code).
    pub fn derived_connections(&self) -> Vec<Connection> {
        let mut conns = Vec::new();
        for (source_id, target_id) in self.import_edges() {
            if let (Some(src), Some(tgt)) = (self.nodes.get(&source_id), self.nodes.get(&target_id)) {
                for out_port in &src.script_outputs {
                    for in_port in &tgt.script_inputs {
                        if out_port.name == in_port.name {
                            conns.push(Connection {
                                from_node: source_id,
                                to_node: target_id,
                                from_port: Some(out_port.name.clone()),
                                to_port: Some(in_port.name.clone()),
                            });
                        }
                    }
                }
            }
        }
        conns
    }

    /// Resolve available inputs for a node.
    /// Includes widget_values + all upstream output values from imported nodes.
    pub fn resolve_all_input_values(&self, node_id: NodeId) -> HashMap<String, Value> {
        let node = match self.nodes.get(&node_id) {
            Some(n) => n,
            None => return HashMap::new(),
        };

        let mut vals = node.input_values.clone();

        // Add upstream output values from imported nodes
        for import_label in extract_imports(&node.script_code) {
            if let Some(source_id) = self.find_node_by_import_label(&import_label) {
                if let Some(src) = self.nodes.get(&source_id) {
                    for (out_name, out_val) in &src.output_values {
                        vals.insert(out_name.clone(), out_val.clone());
                    }
                }
            }
        }

        // Widget values override
        for (k, v) in &node.widget_values {
            vals.insert(k.clone(), v.clone());
        }

        vals
    }

    /// Check if a node has a connected input for a given port name.
    pub fn is_port_connected(&self, node_id: NodeId, port_name: &str) -> bool {
        let node = match self.nodes.get(&node_id) {
            Some(n) => n,
            None => return false,
        };
        for import_label in extract_imports(&node.script_code) {
            if let Some(source_id) = self.find_node_by_import_label(&import_label) {
                if let Some(src) = self.nodes.get(&source_id) {
                    if src.output_values.contains_key(port_name)
                        || src.script_outputs.iter().any(|p| p.name == port_name)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn topological_sort(&self) -> Result<Vec<NodeId>, Vec<NodeId>> {
        let edges = self.import_edges();
        let mut in_degree: HashMap<NodeId, usize> = HashMap::new();
        let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        for id in self.nodes.keys() {
            in_degree.entry(*id).or_insert(0);
            adjacency.entry(*id).or_default();
        }

        for (from, to) in &edges {
            *in_degree.entry(*to).or_insert(0) += 1;
            adjacency.entry(*from).or_default().push(*to);
        }

        let mut queue: Vec<NodeId> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();
        queue.sort();

        let mut order = Vec::new();
        while let Some(id) = queue.pop() {
            order.push(id);
            if let Some(neighbors) = adjacency.get(&id) {
                for &next in neighbors {
                    if let Some(deg) = in_degree.get_mut(&next) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(next);
                        }
                    }
                }
            }
        }

        if order.len() == self.nodes.len() {
            Ok(order)
        } else {
            let cycle_nodes: Vec<NodeId> = self
                .nodes
                .keys()
                .filter(|id| !order.contains(id))
                .copied()
                .collect();
            Err(cycle_nodes)
        }
    }

    /// Find all ancestor nodes (transitive upstream) of the given node, including itself.
    pub fn ancestors_sorted(&self, target: NodeId) -> Vec<NodeId> {
        let edges = self.import_edges();
        let mut needed: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut stack = vec![target];
        while let Some(id) = stack.pop() {
            if needed.insert(id) {
                for (from, to) in &edges {
                    if *to == id {
                        stack.push(*from);
                    }
                }
            }
        }
        match self.topological_sort() {
            Ok(order) => order.into_iter().filter(|id| needed.contains(id)).collect(),
            Err(_) => needed.into_iter().collect(),
        }
    }

    /// Find all descendant nodes (transitive downstream) of the given node, excluding itself.
    pub fn descendants_sorted(&self, source: NodeId) -> Vec<NodeId> {
        let edges = self.import_edges();
        let mut needed: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut stack = vec![source];
        while let Some(id) = stack.pop() {
            for (from, to) in &edges {
                if *from == id && needed.insert(*to) {
                    stack.push(*to);
                }
            }
        }
        needed.remove(&source);
        match self.topological_sort() {
            Ok(order) => order.into_iter().filter(|id| needed.contains(id)).collect(),
            Err(_) => needed.into_iter().collect(),
        }
    }

    /// Find direct downstream node IDs that import the given node.
    pub fn direct_downstream(&self, source_id: NodeId) -> Vec<NodeId> {
        let source_label = match self.nodes.get(&source_id) {
            Some(n) => n.label.replace(' ', "-"),
            None => return Vec::new(),
        };
        let mut result = Vec::new();
        for (&id, node) in &self.nodes {
            if id != source_id && extract_imports(&node.script_code).contains(&source_label) {
                result.push(id);
            }
        }
        result
    }
}

#[derive(Debug, Clone)]
pub struct RunEvent {
    pub node_id: NodeId,
    pub node_name: String,
    pub duration_us: u64,
    pub result: Result<String, String>,
}
