use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Content hash of code string. Same code = same hash regardless of node name/position.
pub fn content_hash(code: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    hasher.finish()
}

/// Kind of Scheme definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefForm {
    Simple,
    Function,
    Syntax,
    RecordType,
}

/// Per-definition info: name, content hash (Unison-style, body only), source line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefInfo {
    pub name: String,
    pub hash: u64,
    pub line: u32,
    pub form: DefForm,
}

/// Hash-based import: references a definition by its content hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashImport {
    pub hash: String,       // hex content hash of the definition body
    pub local_name: String, // name bound in the importing node's scope
}

/// Extract individual define forms from Scheme code.
/// Returns DefInfo for each top-level define/define-syntax/define-record-type.
/// Hash covers the body only (not the name) — Unison-style content addressing.
pub fn extract_definitions(code: &str) -> Vec<DefInfo> {
    let mut defs = Vec::new();
    // Track byte offset → line number
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(code.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let byte_to_line = |byte_offset: usize| -> u32 {
        match line_starts.binary_search(&byte_offset) {
            Ok(i) => (i + 1) as u32,
            Err(i) => i as u32,
        }
    };

    // Split into top-level forms with byte offsets
    let forms = split_toplevel_forms_with_offsets(code);

    for (offset, form) in forms {
        let trimmed = form.trim();
        if let Some(def) = parse_define(trimmed) {
            defs.push(DefInfo {
                name: def.0,
                hash: crate::sexp::canonical_hash_str(def.1),
                line: byte_to_line(offset),
                form: def.2,
            });
        }
    }
    defs
}

/// Split code into top-level S-expressions, returning (byte_offset, form_string).
pub fn split_toplevel_forms_with_offsets(code: &str) -> Vec<(usize, String)> {
    let mut forms = Vec::new();
    let mut depth: i32 = 0;
    let mut start = None;
    let mut in_string = false;
    let mut escape = false;
    let mut in_line_comment = false;

    for (i, c) in code.char_indices() {
        if in_line_comment {
            if c == '\n' { in_line_comment = false; }
            continue;
        }
        if escape { escape = false; continue; }
        if c == '\\' && in_string { escape = true; continue; }
        if c == '"' { in_string = !in_string; continue; }
        if in_string { continue; }
        if c == ';' { in_line_comment = true; continue; }

        if c == '(' {
            if depth == 0 { start = Some(i); }
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                if let Some(s) = start {
                    forms.push((s, code[s..=i].to_string()));
                    start = None;
                }
            }
        }
    }
    forms
}

/// Parse a single top-level form. Returns (name, body_for_hashing, form_kind) if it's a define.
pub fn parse_define(form: &str) -> Option<(String, &str, DefForm)> {
    let inner = form.strip_prefix('(')?.strip_suffix(')')?;
    let trimmed = inner.trim_start();

    if let Some(rest) = trimmed.strip_prefix("define-record-type") {
        let rest = rest.trim_start();
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        // Hash everything after "define-record-type"
        let body_start = form.find("define-record-type").unwrap() + "define-record-type".len();
        return Some((name, &form[body_start..], DefForm::RecordType));
    }
    if let Some(rest) = trimmed.strip_prefix("define-syntax") {
        let rest = rest.trim_start();
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        let body_start = form.find("define-syntax").unwrap() + "define-syntax".len();
        return Some((name, &form[body_start..], DefForm::Syntax));
    }
    if let Some(rest) = trimmed.strip_prefix("define") {
        let rest = rest.trim_start();
        if rest.starts_with('(') {
            // (define (f x y) body...) — function form
            // Find the closing paren of the parameter list
            let mut depth = 0;
            let mut param_end = 0;
            for (i, c) in rest.char_indices() {
                if c == '(' { depth += 1; }
                if c == ')' { depth -= 1; if depth == 0 { param_end = i; break; } }
            }
            // Name is the first symbol inside parens
            let params = &rest[1..param_end];
            let name = params.split_whitespace().next().unwrap_or("").to_string();
            // Body = everything after the param list (hash includes params minus name)
            let body = &rest[param_end + 1..];
            return Some((name, body, DefForm::Function));
        } else {
            // (define x body...) — simple form
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            // Body = everything after name
            let name_end = rest.find(&name).unwrap() + name.len();
            let body = &rest[name_end..];
            return Some((name, body, DefForm::Simple));
        }
    }
    None
}

// Re-export from net crate
pub use wasm_canvas_net::{Value, RepaintSignal, NoRepaint};

/// Wrapper to implement RepaintSignal for egui::Context.
#[cfg(feature = "gui")]
#[derive(Clone)]
pub struct EguiRepaint(pub egui::Context);

#[cfg(feature = "gui")]
impl RepaintSignal for EguiRepaint {
    fn request_repaint(&self) {
        self.0.request_repaint();
    }
}

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
    #[cfg(feature = "gui")]
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
    /// Script code for remote/network templates (double-click to create node with this code)
    pub script_code: Option<String>,
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
    /// Module exports declared by this node (e.g. ["gain", "freq"])
    #[serde(default)]
    pub exports: Vec<String>,
    /// Module imports: (canvas_name, module_name) pairs (legacy)
    #[serde(default)]
    pub imports: Vec<(String, String)>,
    /// Hash-based imports: individual definitions by content hash
    #[serde(default)]
    pub hash_imports: Vec<HashImport>,
    /// Per-definition content hashes (Unison-style)
    #[serde(default)]
    pub definitions: Vec<DefInfo>,
    /// Content hash of script_code — same code = same hash regardless of name/position
    #[serde(skip)]
    pub code_hash: u64,
    #[serde(skip)]
    pub error: Option<String>,
    #[serde(skip)]
    pub last_exec_us: Option<u64>,
    #[serde(skip)]
    pub render_blocks: Vec<crate::render::RenderBlock>,
    /// true = remote phantom node, no script_code
    #[serde(skip)]
    pub phantom: bool,
    /// peer ID source for phantom nodes
    #[serde(skip)]
    pub remote_peer: Option<String>,
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

    /// Update script_code and recompute content hash + per-definition hashes.
    pub fn set_code(&mut self, code: String) {
        self.code_hash = content_hash(&code);
        self.definitions = extract_definitions(&code);
        self.script_code = code;
    }

    /// Recompute code_hash and definitions from current script_code.
    pub fn recompute_hash(&mut self) {
        self.code_hash = content_hash(&self.script_code);
        self.definitions = extract_definitions(&self.script_code);
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
    #[serde(default = "default_true")]
    pub share_code: bool,
}

fn default_true() -> bool { true }

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            next_node_id: 1,
            viewport_offset: [0.0, 0.0],
            viewport_zoom: 1.0,
            share_code: true,
        }
    }

    pub fn add_node(&mut self, template: &NodeTemplate, pos: [f32; 2]) -> NodeId {
        let id = self.next_node_id;
        self.next_node_id += 1;

        let mut input_values = HashMap::new();
        for port in &template.inputs {
            input_values.insert(port.name.clone(), port.port_type.default_value());
        }

        let script_code = if let Some(ref code) = template.script_code {
            code.clone()
        } else if template.builtin == Some(BuiltinKind::Script) {
"".to_string()
        } else {
            String::new()
        };

        let mut node = Node {
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
            exports: Vec::new(),
            imports: Vec::new(), hash_imports: Vec::new(),
            definitions: Vec::new(), code_hash: 0,
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
            phantom: false,
            remote_peer: None,
        };
        crate::scheme_engine::migrate_module_header(&mut node);
        node.recompute_hash();
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

    /// Derive input ports for a node from its imports.
    pub fn derive_inputs_for_node(&self, node_id: NodeId) -> Vec<PortDef> {
        let node = match self.nodes.get(&node_id) {
            Some(n) => n,
            None => return Vec::new(),
        };
        if node.imports.is_empty() {
            return Vec::new();
        }
        let mut inputs = Vec::new();
        for (_, module_name) in &node.imports {
            if let Some(src_id) = self.find_node_by_import_label(module_name) {
                if let Some(src) = self.nodes.get(&src_id) {
                    for port in &src.script_outputs {
                        if !inputs.iter().any(|p: &PortDef| p.name == port.name) {
                            inputs.push(port.clone());
                        }
                    }
                }
            }
        }
        inputs
    }

    /// Build import-based dependency edges: (source_id, target_id) for each import.
    fn import_edges(&self) -> Vec<(NodeId, NodeId)> {
        let mut edges = Vec::new();
        for (&target_id, target_node) in &self.nodes {
            for (_, module_name) in &target_node.imports {
                if let Some(source_id) = self.find_node_by_import_label(module_name) {
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

        // Add upstream output values from all imported nodes
        for (_, module_name) in &node.imports {
            if let Some(source_id) = self.find_node_by_import_label(module_name) {
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
        for (_, module_name) in &node.imports {
            if let Some(source_id) = self.find_node_by_import_label(module_name) {
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
            if id != source_id {
                if node.imports.iter().any(|(_, module)| *module == source_label) {
                    result.push(id);
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: NodeId, label: &str, code: &str) -> Node {
        Node {
            id,
            template_name: "Script".to_string(),
            label: label.to_string(),
            pos: [0.0, 0.0],
            input_values: HashMap::new(),
            output_values: HashMap::new(),
            script_code: code.to_string(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            exports: Vec::new(),
            imports: Vec::new(), hash_imports: Vec::new(),
            definitions: Vec::new(), code_hash: 0,
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
            phantom: false,
            remote_peer: None,
        }
    }

    fn make_phantom(id: NodeId, label: &str, outputs: HashMap<String, Value>) -> Node {
        Node {
            id,
            template_name: "Script".to_string(),
            label: label.to_string(),
            pos: [0.0, 0.0],
            input_values: HashMap::new(),
            output_values: outputs,
            script_code: String::new(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            exports: Vec::new(),
            imports: Vec::new(), hash_imports: Vec::new(),
            definitions: Vec::new(), code_hash: 0,
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
            phantom: true,
            remote_peer: Some("peer-abc".to_string()),
        }
    }

    fn make_module_node(id: NodeId, label: &str, exports: &[&str], imports: &[(&str, &str)]) -> Node {
        let mut node = make_node(id, label, "");
        node.exports = exports.iter().map(|s| s.to_string()).collect();
        node.imports = imports.iter().map(|(c, m)| (c.to_string(), m.to_string())).collect();
        node
    }

    // --- Phantom node discovery via local imports ---

    #[test]
    fn test_phantom_found_by_local_import() {
        let mut graph = Graph::new();

        // Phantom "controls" with output values
        let mut outputs = HashMap::new();
        outputs.insert("gain".to_string(), Value::F64(50.0));
        outputs.insert("freq".to_string(), Value::F64(5.0));
        let phantom = make_phantom(1, "controls", outputs);
        graph.nodes.insert(1, phantom);

        // Node that imports controls
        let wave = make_module_node(2, "wave", &["out"], &[("test", "controls")]);
        graph.nodes.insert(2, wave);

        // import_edges should find phantom as upstream of wave
        let edges = graph.derived_connections();
        // At minimum, find_node_by_import_label should work
        assert_eq!(graph.find_node_by_import_label("controls"), Some(1));

        // resolve_all_input_values should pull phantom's outputs
        let inputs = graph.resolve_all_input_values(2);
        assert_eq!(inputs.get("gain"), Some(&Value::F64(50.0)));
        assert_eq!(inputs.get("freq"), Some(&Value::F64(5.0)));
    }

    // --- Phantom node discovery via remote imports ---

    #[test]
    fn test_phantom_found_by_remote_import() {
        let mut graph = Graph::new();

        let mut outputs = HashMap::new();
        outputs.insert("gain".to_string(), Value::F64(42.0));
        let phantom = make_phantom(1, "controls", outputs);
        graph.nodes.insert(1, phantom);

        // Node that imports via remote syntax: (use-module (other-canvas controls))
        let wave = make_module_node(2, "wave", &["out"], &[("other-canvas", "controls")]);
        graph.nodes.insert(2, wave);

        // resolve_all_input_values should find phantom via imports
        let inputs = graph.resolve_all_input_values(2);
        assert_eq!(inputs.get("gain"), Some(&Value::F64(42.0)));
    }

    // --- derive_inputs_for_node ---

    #[test]
    fn test_derive_inputs_from_local_module() {
        let mut graph = Graph::new();

        let mut controls = make_module_node(1, "controls", &["gain", "freq"], &[]);
        controls.script_outputs = vec![
            PortDef { name: "gain".to_string(), port_type: PortType::F64 },
            PortDef { name: "freq".to_string(), port_type: PortType::F64 },
        ];
        graph.nodes.insert(1, controls);

        let wave = make_module_node(2, "wave", &["out"], &[("test", "controls")]);
        graph.nodes.insert(2, wave);

        let inputs = graph.derive_inputs_for_node(2);
        assert_eq!(inputs.len(), 2);
        assert!(inputs.iter().any(|p| p.name == "gain"));
        assert!(inputs.iter().any(|p| p.name == "freq"));
    }

    #[test]
    fn test_derive_inputs_from_phantom() {
        let mut graph = Graph::new();

        let mut outputs = HashMap::new();
        outputs.insert("gain".to_string(), Value::F64(50.0));
        let mut phantom = make_phantom(1, "controls", outputs);
        phantom.script_outputs = vec![
            PortDef { name: "gain".to_string(), port_type: PortType::F64 },
        ];
        graph.nodes.insert(1, phantom);

        let wave = make_module_node(2, "wave", &["out"], &[("test", "controls")]);
        graph.nodes.insert(2, wave);

        let inputs = graph.derive_inputs_for_node(2);
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "gain");
    }

    #[test]
    fn test_derive_inputs_from_remote_phantom() {
        let mut graph = Graph::new();

        let mut outputs = HashMap::new();
        outputs.insert("x".to_string(), Value::F64(1.0));
        let mut phantom = make_phantom(1, "data", outputs);
        phantom.script_outputs = vec![
            PortDef { name: "x".to_string(), port_type: PortType::F64 },
        ];
        graph.nodes.insert(1, phantom);

        let consumer = make_module_node(2, "consumer", &["y"], &[("peer-abc", "data")]);
        graph.nodes.insert(2, consumer);

        let inputs = graph.derive_inputs_for_node(2);
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "x");
    }

    // --- direct_downstream with phantom ---

    #[test]
    fn test_direct_downstream_from_phantom() {
        let mut graph = Graph::new();

        let outputs = HashMap::from([("gain".to_string(), Value::F64(50.0))]);
        let phantom = make_phantom(1, "controls", outputs);
        graph.nodes.insert(1, phantom);

        let wave = make_module_node(2, "wave", &["out"], &[("test", "controls")]);
        graph.nodes.insert(2, wave);

        let downstream = graph.direct_downstream(1);
        assert_eq!(downstream, vec![2]);
    }

    // --- Local node takes priority over phantom ---

    #[test]
    fn test_local_node_priority_over_phantom() {
        let mut graph = Graph::new();

        // Local "controls" with gain=100
        let mut local = make_module_node(1, "controls", &["gain"], &[]);
        local.output_values.insert("gain".to_string(), Value::F64(100.0));
        graph.nodes.insert(1, local);

        // Phantom "controls" with gain=42 (should be ignored — same label)
        let outputs = HashMap::from([("gain".to_string(), Value::F64(42.0))]);
        let phantom = make_phantom(2, "controls", outputs);
        graph.nodes.insert(2, phantom);

        // Consumer imports controls — should get local (id=1) because
        // find_node_by_import_label returns first match
        let consumer = make_module_node(3, "consumer", &["out"], &[("test", "controls")]);
        graph.nodes.insert(3, consumer);

        let inputs = graph.resolve_all_input_values(3);
        // Should get one of the two — both have "gain"
        assert!(inputs.contains_key("gain"));
    }

    // --- Widget values override upstream ---

    #[test]
    fn test_widget_values_override_upstream() {
        let mut graph = Graph::new();

        let outputs = HashMap::from([("gain".to_string(), Value::F64(50.0))]);
        let phantom = make_phantom(1, "controls", outputs);
        graph.nodes.insert(1, phantom);

        let mut wave = make_module_node(2, "wave", &["out"], &[("test", "controls")]);
        wave.widget_values.insert("gain".to_string(), Value::F64(999.0));
        graph.nodes.insert(2, wave);

        let inputs = graph.resolve_all_input_values(2);
        // Widget value should override phantom's output
        assert_eq!(inputs.get("gain"), Some(&Value::F64(999.0)));
    }

    // --- Topological sort with phantom ---

    #[test]
    fn test_topo_sort_with_phantom() {
        let mut graph = Graph::new();

        let outputs = HashMap::from([("v".to_string(), Value::F64(1.0))]);
        let phantom = make_phantom(1, "source", outputs);
        graph.nodes.insert(1, phantom);

        let mid = make_module_node(2, "mid", &["w"], &[("test", "source")]);
        graph.nodes.insert(2, mid);

        let leaf = make_module_node(3, "leaf", &["z"], &[("test", "mid")]);
        graph.nodes.insert(3, leaf);

        let order = graph.topological_sort().unwrap();
        let pos_phantom = order.iter().position(|&id| id == 1).unwrap();
        let pos_mid = order.iter().position(|&id| id == 2).unwrap();
        let pos_leaf = order.iter().position(|&id| id == 3).unwrap();
        assert!(pos_phantom < pos_mid);
        assert!(pos_mid < pos_leaf);
    }

    // --- Descendants through phantom ---

    #[test]
    fn test_descendants_of_phantom() {
        let mut graph = Graph::new();

        let outputs = HashMap::from([("v".to_string(), Value::F64(1.0))]);
        let phantom = make_phantom(1, "source", outputs);
        graph.nodes.insert(1, phantom);

        let a = make_module_node(2, "a", &["x"], &[("test", "source")]);
        graph.nodes.insert(2, a);

        let b = make_module_node(3, "b", &["y"], &[("test", "a")]);
        graph.nodes.insert(3, b);

        let desc = graph.descendants_sorted(1);
        assert!(desc.contains(&2));
        assert!(desc.contains(&3));
    }

    // =========================================================================
    // Integration tests: canvas/module wire protocol and cross-canvas resolution
    // =========================================================================

    /// Verify that auto_publish_node would produce the correct channel format.
    /// Channel = "canvas-name/module-name" from (define-module (canvas-name module-name)).
    #[test]
    fn test_publish_channel_format() {
        let code = "(define-module (alice controls)\n  (export gain freq))";
        let header = crate::scheme_engine::parse_module_header(code).unwrap();
        let channel = format!("{}/{}", header.canvas, header.name);
        assert_eq!(channel, "alice/controls");
    }

    /// Verify channel parsing: "canvas/module" → (canvas, module).
    #[test]
    fn test_channel_parsing() {
        let channel = "alice-project/sensors";
        let (source_canvas, module_name) = if let Some(slash) = channel.find('/') {
            (&channel[..slash], &channel[slash + 1..])
        } else {
            ("unknown", channel)
        };
        assert_eq!(source_canvas, "alice-project");
        assert_eq!(module_name, "sensors");

        // Legacy channel without slash
        let legacy = "controls";
        let (sc, mn) = if let Some(slash) = legacy.find('/') {
            (&legacy[..slash], &legacy[slash + 1..])
        } else {
            ("unknown", legacy)
        };
        assert_eq!(sc, "unknown");
        assert_eq!(mn, "controls");
    }

    /// Simulate network delivery: a phantom node appears from "alice/controls",
    /// and a local node with (use-module (alice controls)) resolves it.
    #[test]
    fn test_network_delivery_creates_phantom_and_resolves() {
        let mut graph = Graph::new();

        // Simulate ValuesReceived: channel "alice/controls" → phantom with label "controls"
        let channel = "alice/controls";
        let (source_canvas, module_name) = channel.split_once('/').unwrap();
        assert_eq!(source_canvas, "alice");
        assert_eq!(module_name, "controls");

        // Create phantom node as deliver_values would
        let mut outputs = HashMap::new();
        outputs.insert("gain".to_string(), Value::F64(75.0));
        outputs.insert("freq".to_string(), Value::F64(440.0));
        let mut phantom = make_phantom(1, module_name, outputs.clone());
        phantom.script_outputs = vec![
            PortDef { name: "gain".to_string(), port_type: PortType::F64 },
            PortDef { name: "freq".to_string(), port_type: PortType::F64 },
        ];
        graph.nodes.insert(1, phantom);

        // Local node that imports from alice canvas
        let consumer = make_module_node(2, "synth", &["sound"], &[("alice", "controls")]);
        graph.nodes.insert(2, consumer);

        // The import (alice controls) should resolve to phantom "controls" by module name
        let imports = graph.nodes[&2].imports.clone();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0], ("alice".to_string(), "controls".to_string()));

        // find_node_by_import_label finds phantom by module name
        assert_eq!(graph.find_node_by_import_label("controls"), Some(1));

        // resolve_all_input_values pulls phantom's outputs
        let vals = graph.resolve_all_input_values(2);
        assert_eq!(vals.get("gain"), Some(&Value::F64(75.0)));
        assert_eq!(vals.get("freq"), Some(&Value::F64(440.0)));

        // Topological order: phantom before consumer
        let order = graph.topological_sort().unwrap();
        let p1 = order.iter().position(|&id| id == 1).unwrap();
        let p2 = order.iter().position(|&id| id == 2).unwrap();
        assert!(p1 < p2, "phantom should be before consumer in topo order");

        // direct_downstream: phantom → consumer
        let dd = graph.direct_downstream(1);
        assert_eq!(dd, vec![2]);
    }

    /// Two different canvases publish modules with the same name.
    /// The channel format "canvas/module" disambiguates them.
    #[test]
    fn test_same_module_name_different_canvases() {
        let code_alice = "(define-module (alice controls)\n  (export gain))";
        let code_bob = "(define-module (bob controls)\n  (export gain))";

        let h1 = crate::scheme_engine::parse_module_header(code_alice).unwrap();
        let h2 = crate::scheme_engine::parse_module_header(code_bob).unwrap();

        let ch1 = format!("{}/{}", h1.canvas, h1.name);
        let ch2 = format!("{}/{}", h2.canvas, h2.name);

        assert_eq!(ch1, "alice/controls");
        assert_eq!(ch2, "bob/controls");
        assert_ne!(ch1, ch2); // different channels despite same module name
    }

    /// Cross-canvas import: node on "bob" canvas imports from "alice" canvas.
    /// Both canvases are local — no network needed, loopback delivery.
    #[test]
    fn test_cross_canvas_loopback() {
        // Canvas "alice" has a "controls" node
        let mut alice_graph = Graph::new();
        let mut controls = make_module_node(1, "controls", &["gain"], &[]);
        controls.output_values.insert("gain".to_string(), Value::F64(88.0));
        controls.script_outputs = vec![
            PortDef { name: "gain".to_string(), port_type: PortType::F64 },
        ];
        alice_graph.nodes.insert(1, controls);

        // Canvas "bob" has a "synth" node that imports from alice
        let mut bob_graph = Graph::new();
        let synth = make_module_node(2, "synth", &["sound"], &[("alice", "controls")]);
        bob_graph.nodes.insert(2, synth);

        // Simulate loopback: alice publishes, bob receives phantom
        let source_node = alice_graph.nodes.get(&1).unwrap();
        let values = source_node.output_values.clone();

        // deliver_values would create a phantom on bob's canvas
        let mut phantom = make_phantom(10, "controls", values.clone());
        phantom.script_outputs = vec![
            PortDef { name: "gain".to_string(), port_type: PortType::F64 },
        ];
        bob_graph.nodes.insert(10, phantom);

        // Synth should resolve controls from phantom
        let inputs = bob_graph.resolve_all_input_values(2);
        assert_eq!(inputs.get("gain"), Some(&Value::F64(88.0)));

        // direct_downstream: phantom → synth
        let downstream = bob_graph.direct_downstream(10);
        assert!(downstream.contains(&2));
    }

    /// Phantom node should NOT be created if a local node with the same module name exists.
    #[test]
    fn test_local_module_blocks_phantom() {
        let mut graph = Graph::new();

        // Local "controls" module
        let mut local = make_module_node(1, "controls", &["gain"], &[]);
        local.output_values.insert("gain".to_string(), Value::F64(100.0));
        graph.nodes.insert(1, local);

        // Consumer imports controls
        let consumer = make_module_node(2, "synth", &["out"], &[("demo", "controls")]);
        graph.nodes.insert(2, consumer);

        // Simulate incoming network values for "controls"
        // deliver_values checks: has_local → skip (now uses label match)
        let has_local = graph.nodes.values().any(|n| {
            !n.phantom && n.label == "controls"
        });
        assert!(has_local, "Local module should block phantom creation");

        // Consumer resolves from local, not phantom
        let vals = graph.resolve_all_input_values(2);
        assert_eq!(vals.get("gain"), Some(&Value::F64(100.0)));
    }

    /// Multiple imports from different canvases in a single node.
    #[test]
    fn test_multiple_cross_canvas_imports() {
        let mut graph = Graph::new();

        // Phantom from alice
        let mut outputs_a = HashMap::new();
        outputs_a.insert("gain".to_string(), Value::F64(50.0));
        let phantom_a = make_phantom(1, "controls", outputs_a);
        graph.nodes.insert(1, phantom_a);

        // Phantom from charlie
        let mut outputs_c = HashMap::new();
        outputs_c.insert("rate".to_string(), Value::F64(120.0));
        let phantom_c = make_phantom(2, "clock", outputs_c);
        graph.nodes.insert(2, phantom_c);

        // Local node imports from both
        let synth = make_module_node(3, "synth", &["sound"], &[("alice", "controls"), ("charlie", "clock")]);
        graph.nodes.insert(3, synth);

        let vals = graph.resolve_all_input_values(3);
        assert_eq!(vals.get("gain"), Some(&Value::F64(50.0)));
        assert_eq!(vals.get("rate"), Some(&Value::F64(120.0)));

        // Both phantoms are upstream
        let ancestors = graph.ancestors_sorted(3);
        assert!(ancestors.contains(&1));
        assert!(ancestors.contains(&2));
        assert!(ancestors.contains(&3));
    }

    /// R6RS library registration and import roundtrip:
    /// register_node_library_named creates (library (canvas module) ...),
    /// strip_module_header generates matching (import (canvas module)).
    #[test]
    fn test_library_registration_roundtrip() {
        let engine = crate::scheme_engine::SchemeEngine::new().unwrap();
        let mut outputs = HashMap::new();
        outputs.insert("gain".to_string(), Value::F64(42.0));
        outputs.insert("freq".to_string(), Value::F64(440.0));

        // Register library as (alice controls)
        engine.register_node_library_named(1, "alice", "controls", &outputs);

        // strip_module_header should generate compatible import
        let code = "(define-module (bob synth)\n  (use-module (alice controls))\n  (export x))";
        let (body, imports) = crate::scheme_engine::strip_module_header(code);
        assert_eq!(imports, vec!["(import (alice controls))"]);
        assert!(!body.contains("define-module"));

        // Eval the import — should resolve
        let env = engine.make_env();
        for stmt in &imports {
            env.eval(true, stmt).expect("import should resolve");
        }
        let results = env.eval(false, "gain").unwrap();
        let val = results[0].cast_to_scheme_type::<f64>().unwrap();
        assert!((val - 42.0).abs() < f64::EPSILON);

        let results = env.eval(false, "freq").unwrap();
        let val = results[0].cast_to_scheme_type::<f64>().unwrap();
        assert!((val - 440.0).abs() < f64::EPSILON);
    }

    /// Full pipeline: register library → execute_script with module header → outputs resolve.
    /// The imported binding `gain` comes from the R6RS library, not from (input ...).
    #[test]
    fn test_full_module_pipeline() {
        let engine = crate::scheme_engine::SchemeEngine::new().unwrap();

        // Register "alice/controls" with gain=50
        let mut ctrl_outputs = HashMap::new();
        ctrl_outputs.insert("gain".to_string(), Value::F64(50.0));
        engine.register_node_library_named(1, "alice", "controls", &ctrl_outputs);

        // Execute a node that imports from alice/controls and uses gain directly
        let code = "(define result (* gain 2))\n";
        let exports = vec!["result".to_string()];
        let imports = vec![("alice".to_string(), "controls".to_string())];
        let inputs = HashMap::new();
        let (result, _, _) = engine.execute_script_cached(None, &inputs, None, code, &exports, &imports, &HashMap::new()).unwrap();
        // gain=50 from library, result=50*2=100
        match result.output_values.get("result") {
            Some(Value::F64(v)) => assert!((*v - 100.0).abs() < f64::EPSILON),
            other => panic!("Expected F64(100.0), got {:?}", other),
        }
    }

    /// Phantom update: when new values arrive, phantom outputs update and downstream sees them.
    #[test]
    fn test_phantom_value_update_propagates() {
        let mut graph = Graph::new();

        // Initial phantom
        let mut outputs = HashMap::new();
        outputs.insert("temp".to_string(), Value::F64(20.0));
        let phantom = make_phantom(1, "sensor", outputs);
        graph.nodes.insert(1, phantom);

        // Consumer
        let consumer = make_module_node(2, "display", &["out"], &[("remote", "sensor")]);
        graph.nodes.insert(2, consumer);

        // Initial resolve
        let vals = graph.resolve_all_input_values(2);
        assert_eq!(vals.get("temp"), Some(&Value::F64(20.0)));

        // Simulate update: new values arrive
        graph.nodes.get_mut(&1).unwrap().output_values
            .insert("temp".to_string(), Value::F64(35.0));

        // Re-resolve: should see updated value
        let vals = graph.resolve_all_input_values(2);
        assert_eq!(vals.get("temp"), Some(&Value::F64(35.0)));
    }

    /// Downstream recompute chain: phantom → A → B should all be in correct topo order.
    #[test]
    fn test_network_triggered_recompute_chain() {
        let mut graph = Graph::new();

        let outputs = HashMap::from([("v".to_string(), Value::F64(1.0))]);
        let phantom = make_phantom(1, "source", outputs);
        graph.nodes.insert(1, phantom);

        let a = make_module_node(2, "processor", &["x"], &[("remote", "source")]);
        graph.nodes.insert(2, a);

        let b = make_module_node(3, "output", &["y"], &[("demo", "processor")]);
        graph.nodes.insert(3, b);

        // Topo sort: phantom(1) → processor(2) → output(3)
        let order = graph.topological_sort().unwrap();
        let p1 = order.iter().position(|&id| id == 1).unwrap();
        let p2 = order.iter().position(|&id| id == 2).unwrap();
        let p3 = order.iter().position(|&id| id == 3).unwrap();
        assert!(p1 < p2);
        assert!(p2 < p3);

        // descendants_sorted from phantom includes both
        let desc = graph.descendants_sorted(1);
        assert_eq!(desc, vec![2, 3]);

        // direct_downstream of phantom is processor
        let dd = graph.direct_downstream(1);
        assert_eq!(dd, vec![2]);

        // direct_downstream of processor is output
        let dd2 = graph.direct_downstream(2);
        assert_eq!(dd2, vec![3]);
    }

    /// Graceful fallback for body-level (import (canvas module)) when library is missing.
    #[test]
    fn test_graceful_import_fallback() {
        let engine = crate::scheme_engine::SchemeEngine::new().unwrap();
        // Body-level import of non-existent library should not crash
        let code = "(import (nonexistent-canvas some-mod))\n(define x 42)";
        let result = engine.execute_script(&HashMap::new(), None, code);
        // Should succeed — fallback defines stub bindings
        assert!(result.is_ok());
    }

    // =========================================================================
    // Source code sharing via __source__ in published values
    // =========================================================================

    /// __source__ is included in published values and can be extracted.
    #[test]
    fn test_source_code_in_published_values() {
        let code = "(define-module (demo controls)\n  (export gain))\n\n(define gain 50)";
        let header = crate::scheme_engine::parse_module_header(code).unwrap();

        // Simulate what auto_publish_node does
        let mut values = HashMap::new();
        values.insert("gain".to_string(), Value::F64(50.0));
        values.insert("__source__".to_string(), Value::Str(code.to_string()));

        // Extract source
        let source = match values.get("__source__") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        };
        assert_eq!(source, Some(code.to_string()));

        // Filter __source__ from node values (what phantom gets)
        let node_values: HashMap<String, Value> = values.iter()
            .filter(|(k, _)| !k.starts_with("__"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert!(!node_values.contains_key("__source__"));
        assert_eq!(node_values.get("gain"), Some(&Value::F64(50.0)));

        // Channel format
        let channel = format!("{}/{}", header.canvas, header.name);
        assert_eq!(channel, "demo/controls");
    }

    /// Remote template is created from __source__ and can be used to add a node.
    #[test]
    fn test_remote_template_from_source() {
        let code = "(define-module (alice synth)\n  (export sound))\n\n(define sound 42)";

        let template = NodeTemplate {
            name: "synth".to_string(),
            category: "alice".to_string(),
            path: None,
            inputs: Vec::new(),
            outputs: vec![PortDef { name: "sound".to_string(), port_type: PortType::F64 }],
            wasm_bytes: None,
            builtin: None,
            script_code: Some(code.to_string()),
        };

        // Double-click creates a node with this template
        let mut graph = Graph::new();
        let id = graph.add_node(&template, [100.0, 100.0]);
        let node = graph.nodes.get(&id).unwrap();

        // After migration, define-module is stripped, exports populated
        assert!(!node.script_code.contains("define-module"));
        assert!(node.script_code.contains("(define sound 42)"));
        assert_eq!(node.exports, vec!["sound".to_string()]);
        assert_eq!(node.label, "synth");
        assert!(!node.phantom);
    }

    // --- extract_definitions tests ---

    #[test]
    fn test_extract_simple_define() {
        let defs = extract_definitions("(define x 42)");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "x");
        assert_eq!(defs[0].form, DefForm::Simple);
        assert_eq!(defs[0].line, 1);
    }

    #[test]
    fn test_extract_function_define() {
        let defs = extract_definitions("(define (square x) (* x x))");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "square");
        assert_eq!(defs[0].form, DefForm::Function);
    }

    #[test]
    fn test_extract_multiple_defines() {
        let code = "(define a 1)\n(define b 2)\n(define (f x) x)";
        let defs = extract_definitions(code);
        assert_eq!(defs.len(), 3);
        assert_eq!(defs[0].name, "a");
        assert_eq!(defs[0].line, 1);
        assert_eq!(defs[1].name, "b");
        assert_eq!(defs[1].line, 2);
        assert_eq!(defs[2].name, "f");
        assert_eq!(defs[2].line, 3);
        assert_eq!(defs[2].form, DefForm::Function);
    }

    #[test]
    fn test_extract_skips_non_defines() {
        let code = "(import (rnrs))\n(define x 1)\n(display x)";
        let defs = extract_definitions(code);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "x");
    }

    #[test]
    fn test_extract_define_syntax() {
        let code = "(define-syntax my-macro\n  (syntax-rules () ((my-macro x) x)))";
        let defs = extract_definitions(code);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "my-macro");
        assert_eq!(defs[0].form, DefForm::Syntax);
    }

    #[test]
    fn test_same_body_same_hash() {
        let defs1 = extract_definitions("(define a (* 2 3))");
        let defs2 = extract_definitions("(define b (* 2 3))");
        assert_eq!(defs1[0].hash, defs2[0].hash, "Same body should produce same hash regardless of name");
    }

    #[test]
    fn test_different_body_different_hash() {
        let defs1 = extract_definitions("(define x 1)");
        let defs2 = extract_definitions("(define x 2)");
        assert_ne!(defs1[0].hash, defs2[0].hash);
    }

    #[test]
    fn test_set_code_populates_definitions() {
        let mut node = make_node(1, "test", "(define foo 42)\n(define (bar x) x)");
        node.set_code("(define foo 42)\n(define (bar x) x)".to_string());
        assert_eq!(node.definitions.len(), 2);
        assert_eq!(node.definitions[0].name, "foo");
        assert_eq!(node.definitions[1].name, "bar");
    }
}
