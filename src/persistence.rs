use crate::db::Db;
use crate::types::{Connection, Graph, Node};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Derive the DB dump path from a graph file path: graph.json → graph.db.json
fn db_path_for(graph_path: &Path) -> PathBuf {
    let stem = graph_path.file_stem().unwrap_or_default().to_string_lossy();
    graph_path.with_file_name(format!("{}.db.json", stem))
}

pub fn save_graph(graph: &Graph, path: &Path, db: &Db) -> Result<()> {
    // Filter out phantom nodes before saving
    let mut save_graph = graph.clone();
    save_graph.nodes.retain(|_, n| !n.phantom);
    let json = serde_json::to_string_pretty(&save_graph)?;
    std::fs::write(path, json)?;

    // Save DB alongside
    let db_json = db.export()?;
    std::fs::write(db_path_for(path), db_json)?;

    Ok(())
}

pub fn load_graph(path: &Path, db: &Db) -> Result<Graph> {
    let json = std::fs::read_to_string(path)?;
    let graph: Graph = serde_json::from_str(&json)?;

    // Load DB if exists alongside
    let db_path = db_path_for(path);
    if db_path.exists() {
        let db_json = std::fs::read_to_string(&db_path)?;
        db.import(&db_json)?;
        log::info!("Loaded DB from {}", db_path.display());
    }

    Ok(graph)
}

/// Save just the DB to a default location (for auto-save on exit)
pub fn save_db(db: &Db, path: &Path) -> Result<()> {
    let db_json = db.export()?;
    std::fs::write(path, db_json)?;
    Ok(())
}

/// Load just the DB from a default location (for auto-load on start)
pub fn load_db(db: &Db, path: &Path) -> Result<()> {
    if path.exists() {
        let db_json = std::fs::read_to_string(path)?;
        db.import(&db_json)?;
        log::info!("Restored DB from {}", path.display());
    }
    Ok(())
}

// --- DB graph persistence (multi-canvas) ---

const DEFAULT_CANVAS: &str = "default";

/// Save graph into the DB under a canvas name.
pub fn save_canvas_to_db(canvas_name: &str, graph: &Graph, db: &Db) -> Result<()> {
    let cn = Db::escape_surql(canvas_name);

    // Clear existing data for this canvas
    db.run(&format!("DELETE graph_meta WHERE canvas = '{}'", cn))?;
    db.run(&format!("DELETE graph_nodes WHERE canvas = '{}'", cn))?;

    // Save metadata
    db.run(&format!(
        "CREATE graph_meta SET canvas = '{}', viewport_offset_x = {}, viewport_offset_y = {}, viewport_zoom = {}, next_node_id = {}, share_code = {}",
        cn, graph.viewport_offset[0], graph.viewport_offset[1], graph.viewport_zoom, graph.next_node_id, graph.share_code
    ))?;

    // Save each non-phantom node
    for (_, node) in &graph.nodes {
        if node.phantom { continue; }
        let input_values_json = serde_json::to_string(&node.input_values)?;
        let widget_values_json = serde_json::to_string(&node.widget_values)?;
        let script_code_escaped = Db::escape_surql(&node.script_code);
        let label_escaped = Db::escape_surql(&node.label);
        let template_escaped = Db::escape_surql(&node.template_name);

        db.run(&format!(
            "CREATE graph_nodes SET \
             canvas = '{}', \
             node_id = {}, \
             template_name = '{}', \
             label = '{}', \
             pos_x = {}, \
             pos_y = {}, \
             script_code = '{}', \
             input_values_json = '{}', \
             widget_values_json = '{}'",
            cn,
            node.id,
            template_escaped,
            label_escaped,
            node.pos[0],
            node.pos[1],
            script_code_escaped,
            Db::escape_surql(&input_values_json),
            Db::escape_surql(&widget_values_json),
        ))?;
    }

    log::info!("Saved canvas '{}' to DB: {} nodes", canvas_name,
        graph.nodes.values().filter(|n| !n.phantom).count());
    Ok(())
}

/// Load graph from the DB by canvas name. Returns None if not found.
pub fn load_canvas_from_db(canvas_name: &str, db: &Db) -> Result<Option<Graph>> {
    let cn = Db::escape_surql(canvas_name);

    let meta_rows = db.query(&format!(
        "SELECT * FROM graph_meta WHERE canvas = '{}'", cn
    ))?;
    let meta = match meta_rows.first() {
        Some(m) => m,
        None => return Ok(None),
    };

    let mut graph = Graph::new();
    graph.viewport_offset[0] = meta.get("viewport_offset_x")
        .and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    graph.viewport_offset[1] = meta.get("viewport_offset_y")
        .and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    graph.viewport_zoom = meta.get("viewport_zoom")
        .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    graph.next_node_id = meta.get("next_node_id")
        .and_then(|v| v.as_u64()).unwrap_or(1);
    graph.share_code = meta.get("share_code")
        .and_then(|v| v.as_bool()).unwrap_or(true);

    let node_rows = db.query(&format!(
        "SELECT * FROM graph_nodes WHERE canvas = '{}'", cn
    ))?;
    for row in &node_rows {
        if let Some(node) = parse_node_row(row) {
            graph.nodes.insert(node.id, node);
        }
    }

    // graph_meta exists → canvas exists (even if empty)
    log::info!("Loaded canvas '{}' from DB: {} nodes", canvas_name, graph.nodes.len());
    Ok(Some(graph))
}

/// List all canvas names in the DB.
pub fn list_canvases(db: &Db) -> Vec<String> {
    db.query("SELECT canvas FROM graph_meta")
        .unwrap_or_default()
        .iter()
        .filter_map(|row| row.get("canvas").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect()
}

/// Delete a canvas from the DB.
pub fn delete_canvas(canvas_name: &str, db: &Db) -> Result<()> {
    let cn = Db::escape_surql(canvas_name);
    db.run(&format!("DELETE graph_meta WHERE canvas = '{}'", cn))?;
    db.run(&format!("DELETE graph_nodes WHERE canvas = '{}'", cn))?;
    log::info!("Deleted canvas '{}' from DB", canvas_name);
    Ok(())
}

/// Backward-compat wrappers
pub fn save_graph_to_db(graph: &Graph, db: &Db) -> Result<()> {
    save_canvas_to_db(DEFAULT_CANVAS, graph, db)
}

pub fn load_graph_from_db(db: &Db) -> Result<Option<Graph>> {
    load_canvas_from_db(DEFAULT_CANVAS, db)
}

/// Parse a single node row from DB query result.
fn parse_node_row(row: &serde_json::Value) -> Option<Node> {
    let node_id = row.get("node_id").and_then(|v| v.as_u64())?;
    let template_name = row.get("template_name")
        .and_then(|v| v.as_str()).unwrap_or("Script").to_string();
    let label = row.get("label")
        .and_then(|v| v.as_str()).unwrap_or("").to_string();
    let pos_x = row.get("pos_x")
        .and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let pos_y = row.get("pos_y")
        .and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let script_code = row.get("script_code")
        .and_then(|v| v.as_str()).unwrap_or("").to_string();

    let input_values: HashMap<String, crate::types::Value> = row.get("input_values_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let widget_values: HashMap<String, crate::types::Value> = row.get("widget_values_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    Some(Node {
        id: node_id,
        template_name,
        label,
        pos: [pos_x, pos_y],
        input_values,
        output_values: HashMap::new(),
        script_code,
        script_inputs: Vec::new(),
        script_outputs: Vec::new(),
        widget_decls: Vec::new(),
        widget_values,
        error: None,
        last_exec_us: None,
        render_blocks: Vec::new(),
        phantom: false,
        remote_peer: None,
    })
}

// --- Favorites ---

/// Save a node as a favorite (by label). Stores script_code + widget_values.
pub fn save_favorite(label: &str, script_code: &str, widget_values: &HashMap<String, crate::types::Value>, db: &Db) -> Result<()> {
    let lbl = Db::escape_surql(label);
    let code = Db::escape_surql(script_code);
    let wv_json = serde_json::to_string(widget_values)?;
    db.run(&format!("DELETE favorites WHERE label = '{}'", lbl))?;
    db.run(&format!(
        "CREATE favorites SET label = '{}', script_code = '{}', widget_values_json = '{}'",
        lbl, code, Db::escape_surql(&wv_json)
    ))?;
    Ok(())
}

/// List all favorite names.
pub fn list_favorites(db: &Db) -> Vec<String> {
    db.query("SELECT label FROM favorites ORDER BY label")
        .unwrap_or_default()
        .iter()
        .filter_map(|row| row.get("label").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect()
}

/// Load a favorite by label.
pub fn load_favorite(label: &str, db: &Db) -> Option<(String, HashMap<String, crate::types::Value>)> {
    let lbl = Db::escape_surql(label);
    let rows = db.query(&format!("SELECT * FROM favorites WHERE label = '{}'", lbl)).ok()?;
    let row = rows.first()?;
    let script_code = row.get("script_code").and_then(|v| v.as_str())?.to_string();
    let widget_values: HashMap<String, crate::types::Value> = row.get("widget_values_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Some((script_code, widget_values))
}

/// Remove a favorite by label.
pub fn remove_favorite(label: &str, db: &Db) -> Result<()> {
    let lbl = Db::escape_surql(label);
    db.run(&format!("DELETE favorites WHERE label = '{}'", lbl))?;
    Ok(())
}

// --- SCM format support ---

/// Load a graph from .scm format
pub fn load_graph_scm(path: &Path, db: &Db) -> Result<Graph> {
    let src = std::fs::read_to_string(path)?;
    let graph = parse_scm(&src)?;

    let db_path = db_path_for(path);
    if db_path.exists() {
        let db_json = std::fs::read_to_string(&db_path)?;
        db.import(&db_json)?;
        log::info!("Loaded DB from {}", db_path.display());
    }

    Ok(graph)
}

/// Save a graph in .scm format
pub fn save_graph_scm(graph: &Graph, path: &Path, db: &Db) -> Result<()> {
    // Filter out phantom nodes before saving
    let mut save_graph = graph.clone();
    save_graph.nodes.retain(|_, n| !n.phantom);
    let scm = serialize_scm(&save_graph);
    std::fs::write(path, scm)?;

    let db_json = db.export()?;
    std::fs::write(db_path_for(path), db_json)?;

    Ok(())
}

/// Find the end of a top-level S-expression starting at `start` (which should point to '(').
/// Tracks parentheses, ignoring those inside strings and ;-comments.
fn find_sexp_end(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b';' => {
                // skip to end of line
                while i < bytes.len() && bytes[i] != b'\n' { i += 1; }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' { i += 2; continue; }
                    if bytes[i] == b'"' { break; }
                    i += 1;
                }
                i += 1; // skip closing quote
                continue;
            }
            b'(' => { depth += 1; i += 1; }
            b')' => {
                depth -= 1;
                if depth == 0 { return Some(i); }
                i += 1;
            }
            _ => { i += 1; }
        }
    }
    None
}

/// Parse a quoted string token from position (must start with '"').
fn parse_quoted(src: &str, pos: usize) -> (String, usize) {
    let bytes = src.as_bytes();
    let mut i = pos + 1; // skip opening quote
    let mut result = String::new();
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => result.push('\n'),
                b't' => result.push('\t'),
                b'\\' => result.push('\\'),
                b'"' => result.push('"'),
                c => { result.push('\\'); result.push(c as char); }
            }
            i += 2;
        } else if bytes[i] == b'"' {
            return (result, i + 1);
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    (result, i)
}

/// Skip whitespace, return new position.
fn skip_ws(src: &str, pos: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = pos;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
    i
}

/// Read a token (unquoted word or number) from position.
fn read_token(src: &str, pos: usize) -> (&str, usize) {
    let bytes = src.as_bytes();
    let start = pos;
    let mut i = pos;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace()
        && bytes[i] != b'(' && bytes[i] != b')' && bytes[i] != b'"'
    {
        i += 1;
    }
    (&src[start..i], i)
}

/// Parsed legacy connection for backward compat conversion.
struct LegacyConnection {
    from_node: u64,
    to_node: u64,
}

fn parse_scm(src: &str) -> Result<Graph> {
    let mut graph = Graph::new();
    let mut max_node_id: u64 = 0;
    let mut legacy_connections: Vec<LegacyConnection> = Vec::new();

    // Find all top-level ( ... ) forms
    let bytes = src.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        // skip whitespace and comments
        if bytes[pos].is_ascii_whitespace() { pos += 1; continue; }
        if bytes[pos] == b';' {
            while pos < bytes.len() && bytes[pos] != b'\n' { pos += 1; }
            continue;
        }
        if bytes[pos] != b'(' {
            pos += 1;
            continue;
        }

        let end = find_sexp_end(src, pos)
            .ok_or_else(|| anyhow::anyhow!("Unmatched paren at offset {}", pos))?;
        let form = &src[pos..=end];

        // Peek first token
        let inner_start = skip_ws(src, pos + 1);
        let (tag, _) = read_token(src, inner_start);

        match tag {
            "graph" => {
                parse_graph_form(form, &mut graph)?;
            }
            "node" => {
                let node = parse_node_form(src, pos, end)?;
                if node.id > max_node_id { max_node_id = node.id; }
                graph.nodes.insert(node.id, node);
            }
            "connection" => {
                // Backward compat: parse legacy connections, convert to imports
                if let Ok(conn) = parse_connection_form(form) {
                    legacy_connections.push(LegacyConnection {
                        from_node: conn.from_node,
                        to_node: conn.to_node,
                    });
                }
            }
            _ => {
                log::warn!("Unknown top-level form: {}", tag);
            }
        }

        pos = end + 1;
    }

    graph.next_node_id = max_node_id + 1;

    // Convert legacy connections to imports in target node script_code
    for lc in &legacy_connections {
        if let Some(source_label) = graph.nodes.get(&lc.from_node).map(|n| n.label.replace(' ', "-")) {
            if let Some(target_node) = graph.nodes.get_mut(&lc.to_node) {
                let import_line = format!("(import (node {}))", source_label);
                if !target_node.script_code.contains(&import_line) {
                    target_node.script_code = format!("{}\n{}", import_line, target_node.script_code);
                }
            }
        }
    }

    Ok(graph)
}

/// Parse (graph (viewport ox oy zoom))
fn parse_graph_form(form: &str, graph: &mut Graph) -> Result<()> {
    // Look for (viewport ...)
    if let Some(vp_start) = form.find("(viewport") {
        let vp_end = find_sexp_end(form, vp_start)
            .ok_or_else(|| anyhow::anyhow!("Bad viewport in graph form"))?;
        let vp_inner = &form[vp_start + 9..vp_end]; // after "(viewport"
        let tokens: Vec<&str> = vp_inner.split_whitespace().collect();
        if tokens.len() >= 3 {
            graph.viewport_offset[0] = tokens[0].parse().unwrap_or(0.0);
            graph.viewport_offset[1] = tokens[1].parse().unwrap_or(0.0);
            graph.viewport_zoom = tokens[2].parse().unwrap_or(1.0);
        }
    }
    Ok(())
}

/// Parse (node ID "Template" "label" (pos X Y) ...body...)
/// The body is everything after the (pos ...) up to the final closing paren.
fn parse_node_form(src: &str, form_start: usize, form_end: usize) -> Result<Node> {
    let mut p = skip_ws(src, form_start + 1); // skip '('
    // skip "node"
    let (_, p2) = read_token(src, p);
    p = skip_ws(src, p2);

    // ID
    let (id_str, p2) = read_token(src, p);
    let id: u64 = id_str.parse().map_err(|_| anyhow::anyhow!("Bad node id: {}", id_str))?;
    p = skip_ws(src, p2);

    // Template name (quoted)
    let (template_name, p2) = parse_quoted(src, p);
    p = skip_ws(src, p2);

    // Label (quoted)
    let (label, p2) = parse_quoted(src, p);
    p = skip_ws(src, p2);

    // (pos X Y)
    let mut pos_xy = [0.0f32; 2];
    if src.as_bytes().get(p) == Some(&b'(') {
        let pos_end = find_sexp_end(src, p)
            .ok_or_else(|| anyhow::anyhow!("Bad pos in node {}", id))?;
        let pos_inner = &src[p + 1..pos_end];
        let tokens: Vec<&str> = pos_inner.split_whitespace().collect();
        // tokens[0] = "pos"
        if tokens.len() >= 3 {
            pos_xy[0] = tokens[1].parse().unwrap_or(0.0);
            pos_xy[1] = tokens[2].parse().unwrap_or(0.0);
        }
        p = pos_end + 1;
    }

    // Parse optional (inputs ...) and (widgets ...) blocks before the script body
    let mut input_values: HashMap<String, crate::types::Value> = HashMap::new();
    let mut widget_values: HashMap<String, crate::types::Value> = HashMap::new();

    p = skip_ws(src, p);
    loop {
        p = skip_ws(src, p);
        if p >= form_end || src.as_bytes()[p] != b'(' { break; }
        // Peek at tag without consuming
        let inner = skip_ws(src, p + 1);
        let (tag, _) = read_token(src, inner);
        if tag != "inputs" && tag != "widgets" { break; }

        let block_end = find_sexp_end(src, p)
            .ok_or_else(|| anyhow::anyhow!("Bad {} block in node {}", tag, id))?;
        let map = if tag == "inputs" { &mut input_values } else { &mut widget_values };
        parse_kv_pairs(&src[p + 1..block_end], map)?;
        p = block_end + 1;
    }

    // Everything from here to form_end is the script body
    p = skip_ws_preserve_newlines(src, p);
    let body = &src[p..form_end];
    let script_code = body.trim().to_string();

    Ok(Node {
        id,
        template_name,
        label,
        pos: pos_xy,
        input_values,
        output_values: HashMap::new(),
        script_code,
        script_inputs: Vec::new(),
        script_outputs: Vec::new(),
        widget_decls: Vec::new(),
        widget_values,
        error: None,
        last_exec_us: None,
        render_blocks: Vec::new(),
        phantom: false,
        remote_peer: None,
    })
}

/// Parse key-value pairs from inside (inputs/widgets ...) block.
/// Format: inputs ("key" value) ("key2" value2) ...
/// Values: numbers, #t/#f, "strings"
fn parse_kv_pairs(inner: &str, map: &mut HashMap<String, crate::types::Value>) -> Result<()> {
    let bytes = inner.as_bytes();
    // Skip the tag word ("inputs" or "widgets")
    let mut p = skip_ws(inner, 0);
    let (_, p2) = read_token(inner, p);
    p = skip_ws(inner, p2);

    // Parse each ("key" value) pair
    while p < bytes.len() {
        p = skip_ws(inner, p);
        if p >= bytes.len() || bytes[p] != b'(' { break; }
        let pair_end = find_sexp_end(inner, p)
            .ok_or_else(|| anyhow::anyhow!("Bad kv pair"))?;

        let mut q = skip_ws(inner, p + 1);
        let (key, q2) = parse_quoted(inner, q);
        q = skip_ws(inner, q2);
        let val = parse_scm_value(inner, q)?;
        map.insert(key, val);

        p = pair_end + 1;
    }
    Ok(())
}

/// Parse a single Value from scm text at position.
fn parse_scm_value(src: &str, pos: usize) -> Result<crate::types::Value> {
    let bytes = src.as_bytes();
    let p = skip_ws(src, pos);
    if p >= bytes.len() {
        return Err(anyhow::anyhow!("Unexpected end of input parsing value"));
    }
    if bytes[p] == b'"' {
        let (s, _) = parse_quoted(src, p);
        return Ok(crate::types::Value::Str(s));
    }
    if bytes[p] == b'#' && p + 1 < bytes.len() {
        return match bytes[p + 1] {
            b't' => Ok(crate::types::Value::Bool(true)),
            b'f' => Ok(crate::types::Value::Bool(false)),
            _ => Err(anyhow::anyhow!("Bad boolean")),
        };
    }
    // Number
    let (tok, _) = read_token(src, p);
    if let Ok(v) = tok.parse::<f64>() {
        Ok(crate::types::Value::F64(v))
    } else if let Ok(v) = tok.parse::<i64>() {
        Ok(crate::types::Value::I64(v))
    } else {
        Err(anyhow::anyhow!("Cannot parse value: {}", tok))
    }
}

/// Format a Value for .scm serialization.
fn value_to_scm(val: &crate::types::Value) -> String {
    use crate::types::Value;
    match val {
        Value::F64(v) => format!("{}", v),
        Value::F32(v) => format!("{}", v),
        Value::I64(v) => format!("{}", v),
        Value::I32(v) => format!("{}", v),
        Value::Bool(b) => if *b { "#t" } else { "#f" }.to_string(),
        Value::Str(s) => format!("{:?}", s), // Rust's Debug gives proper escaping
    }
}

/// Skip only spaces/tabs (not newlines) then skip exactly one newline if present.
fn skip_ws_preserve_newlines(src: &str, pos: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = pos;
    // skip spaces and tabs
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    // skip one newline
    if i < bytes.len() && bytes[i] == b'\n' { i += 1; }
    else if i + 1 < bytes.len() && bytes[i] == b'\r' && bytes[i + 1] == b'\n' { i += 2; }
    i
}

/// Parse legacy (connection ID (from NODE "port") (to NODE "port"))
fn parse_connection_form(form: &str) -> Result<Connection> {
    let mut p = skip_ws(form, 1); // skip '('
    let (_, p2) = read_token(form, p); // skip "connection"
    p = skip_ws(form, p2);

    let (_id_str, p2) = read_token(form, p); // skip ID
    p = skip_ws(form, p2);

    // (from NODE "port")
    let from_end = find_sexp_end(form, p)
        .ok_or_else(|| anyhow::anyhow!("Bad from in connection"))?;
    let (from_node, _from_port) = parse_endpoint(&form[p..=from_end])?;
    p = skip_ws(form, from_end + 1);

    // (to NODE "port")
    let to_end = find_sexp_end(form, p)
        .ok_or_else(|| anyhow::anyhow!("Bad to in connection"))?;
    let (to_node, _to_port) = parse_endpoint(&form[p..=to_end])?;

    Ok(Connection { from_node, to_node, from_port: None, to_port: None })
}

/// Parse (from/to NODE "port") → (node_id, port_name)
fn parse_endpoint(form: &str) -> Result<(u64, String)> {
    let mut p = skip_ws(form, 1); // skip '('
    let (_, p2) = read_token(form, p); // skip "from"/"to"
    p = skip_ws(form, p2);

    let (node_str, p2) = read_token(form, p);
    let node_id: u64 = node_str.parse().map_err(|_| anyhow::anyhow!("Bad node id in endpoint: {}", node_str))?;
    p = skip_ws(form, p2);

    let (port, _) = parse_quoted(form, p);
    Ok((node_id, port))
}

/// Serialize graph to .scm format
fn serialize_scm(graph: &Graph) -> String {
    let mut out = String::from(";;; wasm-canvas graph\n\n");

    // Graph settings
    out.push_str(&format!(
        "(graph\n  (viewport {} {} {}))\n",
        graph.viewport_offset[0], graph.viewport_offset[1], graph.viewport_zoom
    ));

    out.push_str("\n;;; --- nodes ---\n");

    // Sort nodes by id for stable output
    let mut node_ids: Vec<_> = graph.nodes.keys().cloned().collect();
    node_ids.sort();

    for id in node_ids {
        let node = &graph.nodes[&id];
        out.push_str(&format!(
            "\n(node {} {:?} {:?} (pos {} {})\n",
            node.id, node.template_name, node.label,
            node.pos[0], node.pos[1]
        ));

        // Write input_values if non-empty
        if !node.input_values.is_empty() {
            out.push_str("  (inputs");
            let mut keys: Vec<_> = node.input_values.keys().collect();
            keys.sort();
            for k in keys {
                out.push_str(&format!(" ({:?} {})", k, value_to_scm(&node.input_values[k])));
            }
            out.push_str(")\n");
        }

        // Write widget_values if non-empty
        if !node.widget_values.is_empty() {
            out.push_str("  (widgets");
            let mut keys: Vec<_> = node.widget_values.keys().collect();
            keys.sort();
            for k in keys {
                out.push_str(&format!(" ({:?} {})", k, value_to_scm(&node.widget_values[k])));
            }
            out.push_str(")\n");
        }

        // Indent each line of script_code by 2 spaces
        for line in node.script_code.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }

        out.push_str(")\n");
    }

    out
}

pub struct UndoHistory {
    states: Vec<String>,
    current: usize,
    max_size: usize,
}

impl UndoHistory {
    pub fn new(max_size: usize) -> Self {
        Self {
            states: Vec::new(),
            current: 0,
            max_size,
        }
    }

    pub fn push(&mut self, graph: &Graph) {
        let json = serde_json::to_string(graph).unwrap_or_default();

        // If we're not at the end, truncate future states
        if self.current < self.states.len() {
            self.states.truncate(self.current);
        }

        self.states.push(json);
        if self.states.len() > self.max_size {
            self.states.remove(0);
        }
        self.current = self.states.len();
    }

    pub fn undo(&mut self) -> Option<Graph> {
        if self.current > 1 {
            self.current -= 1;
            serde_json::from_str(&self.states[self.current - 1]).ok()
        } else {
            None
        }
    }

    pub fn redo(&mut self) -> Option<Graph> {
        if self.current < self.states.len() {
            self.current += 1;
            serde_json::from_str(&self.states[self.current - 1]).ok()
        } else {
            None
        }
    }

    pub fn can_undo(&self) -> bool {
        self.current > 1
    }

    pub fn can_redo(&self) -> bool {
        self.current < self.states.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scm_roundtrip() {
        let src = r#";;; wasm-canvas graph

(graph
  (viewport 10.0 20.0 1.5))

(node 1 "Script" "hello" (pos 50.0 100.0)
  (inputs ("gain" 42.5) ("name" "test"))
  (widgets ("speed" 3.0))

  (define x 42)

  # Title

  Value = @x
)

(node 2 "Script" "world" (pos 200.0 100.0)

  (define y (input 'x 'f64))
)

(connection 1 (from 1 "out") (to 2 "in"))
"#;
        let graph = parse_scm(src).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.viewport_offset, [10.0, 20.0]);
        assert!((graph.viewport_zoom - 1.5).abs() < 0.01);

        let node1 = &graph.nodes[&1];
        assert_eq!(node1.label, "hello");
        assert_eq!(node1.pos, [50.0, 100.0]);
        assert!(node1.script_code.contains("(define x 42)"));
        assert!(node1.script_code.contains("# Title"));
        assert!(node1.script_code.contains("Value = @x"));
        // Check input_values
        match node1.input_values.get("gain") {
            Some(crate::types::Value::F64(v)) => assert!((v - 42.5).abs() < 0.01),
            other => panic!("Expected F64(42.5), got {:?}", other),
        }
        match node1.input_values.get("name") {
            Some(crate::types::Value::Str(s)) => assert_eq!(s, "test"),
            other => panic!("Expected Str(\"test\"), got {:?}", other),
        }
        // Check widget_values
        match node1.widget_values.get("speed") {
            Some(crate::types::Value::F64(v)) => assert!((v - 3.0).abs() < 0.01),
            other => panic!("Expected F64(3.0), got {:?}", other),
        }

        let node2 = &graph.nodes[&2];
        assert_eq!(node2.template_name, "Script");
        // Legacy connection should have been converted to import in node2's code
        assert!(node2.script_code.contains("(import (node hello))"));

        assert_eq!(graph.next_node_id, 3);
    }

    #[test]
    fn test_serialize_scm() {
        let src = r#"(graph (viewport 0.0 0.0 1.0))

(node 1 "Script" "test" (pos 10.0 20.0)
  (inputs ("gain" 55.0) ("label" "hi"))
  (widgets ("speed" 2.0))

  (define x 1)

  # Hello
)
"#;
        let graph = parse_scm(src).unwrap();
        let serialized = serialize_scm(&graph);
        assert!(serialized.contains("(inputs"));
        assert!(serialized.contains("\"gain\" 55"));
        assert!(serialized.contains("(widgets"));
        assert!(serialized.contains("\"speed\" 2"));
        assert!(serialized.contains("(define x 1)"));
        assert!(serialized.contains("# Hello"));

        // Re-parse and verify values survived
        let graph2 = parse_scm(&serialized).unwrap();
        assert_eq!(graph2.nodes.len(), 1);
        let n = &graph2.nodes[&1];
        assert_eq!(n.label, "test");
        match n.input_values.get("gain") {
            Some(crate::types::Value::F64(v)) => assert!((v - 55.0).abs() < 0.01),
            other => panic!("Expected F64(55.0), got {:?}", other),
        }
        match n.input_values.get("label") {
            Some(crate::types::Value::Str(s)) => assert_eq!(s, "hi"),
            other => panic!("Expected Str(\"hi\"), got {:?}", other),
        }
        match n.widget_values.get("speed") {
            Some(crate::types::Value::F64(v)) => assert!((v - 2.0).abs() < 0.01),
            other => panic!("Expected F64(2.0), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_demo_scm() {
        let src = std::fs::read_to_string("demo.scm").unwrap();
        let graph = parse_scm(&src).unwrap();
        assert_eq!(graph.nodes.len(), 4);

        let wave = &graph.nodes[&4];
        assert_eq!(wave.label, "wave");
        assert!(wave.script_code.contains("canvas"));
        assert!(wave.script_code.contains("draw-polyline"));

        let controls = &graph.nodes[&1];
        assert_eq!(controls.label, "controls");
        assert!(controls.script_code.contains("define-module"));
    }

    #[test]
    fn test_db_graph_roundtrip() {
        let db = crate::db::Db::new().unwrap();

        // Build a graph with various node types
        let mut graph = Graph::new();
        let mut node = Node {
            id: 1,
            template_name: "Script".to_string(),
            label: "test-node".to_string(),
            pos: [100.0, 200.0],
            input_values: HashMap::new(),
            output_values: HashMap::new(),
            script_code: "(define x 42)\n(define y (+ x 1))".to_string(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
            phantom: false,
            remote_peer: None,
        };
        node.input_values.insert("gain".into(), crate::types::Value::F64(55.0));
        node.input_values.insert("name".into(), crate::types::Value::Str("hello".into()));
        node.widget_values.insert("speed".into(), crate::types::Value::F64(3.0));
        graph.nodes.insert(1, node);

        graph.viewport_offset = [10.0, 20.0];
        graph.viewport_zoom = 1.5;
        graph.next_node_id = 2;

        // Save to DB
        save_graph_to_db(&graph, &db).unwrap();

        // Load from DB
        let loaded = load_graph_from_db(&db).unwrap().unwrap();
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(loaded.viewport_offset, [10.0, 20.0]);
        assert!((loaded.viewport_zoom - 1.5).abs() < 0.01);
        assert_eq!(loaded.next_node_id, 2);

        let n = &loaded.nodes[&1];
        assert_eq!(n.label, "test-node");
        assert_eq!(n.pos, [100.0, 200.0]);
        assert!(n.script_code.contains("(define x 42)"));
        match n.input_values.get("gain") {
            Some(crate::types::Value::F64(v)) => assert!((v - 55.0).abs() < 0.01),
            other => panic!("Expected F64(55.0), got {:?}", other),
        }
        match n.input_values.get("name") {
            Some(crate::types::Value::Str(s)) => assert_eq!(s, "hello"),
            other => panic!("Expected Str(\"hello\"), got {:?}", other),
        }
        match n.widget_values.get("speed") {
            Some(crate::types::Value::F64(v)) => assert!((v - 3.0).abs() < 0.01),
            other => panic!("Expected F64(3.0), got {:?}", other),
        }
    }

    #[test]
    fn test_db_graph_phantom_not_saved() {
        let db = crate::db::Db::new().unwrap();

        let mut graph = Graph::new();
        // Regular node
        graph.nodes.insert(1, Node {
            id: 1,
            template_name: "Script".to_string(),
            label: "real".to_string(),
            pos: [0.0, 0.0],
            input_values: HashMap::new(),
            output_values: HashMap::new(),
            script_code: String::new(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
            phantom: false,
            remote_peer: None,
        });
        // Phantom node
        graph.nodes.insert(2, Node {
            id: 2,
            template_name: "Script".to_string(),
            label: "phantom".to_string(),
            pos: [0.0, 0.0],
            input_values: HashMap::new(),
            output_values: HashMap::new(),
            script_code: String::new(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
            phantom: true,
            remote_peer: Some("peer123".to_string()),
        });
        graph.next_node_id = 3;

        save_graph_to_db(&graph, &db).unwrap();
        let loaded = load_graph_from_db(&db).unwrap().unwrap();
        assert_eq!(loaded.nodes.len(), 1);
        assert!(loaded.nodes.contains_key(&1));
        assert!(!loaded.nodes.contains_key(&2));
    }

    #[test]
    fn test_db_graph_special_chars_in_code() {
        let db = crate::db::Db::new().unwrap();

        let mut graph = Graph::new();
        graph.nodes.insert(1, Node {
            id: 1,
            template_name: "Script".to_string(),
            label: "node with 'quotes'".to_string(),
            pos: [0.0, 0.0],
            input_values: HashMap::new(),
            output_values: HashMap::new(),
            script_code: "(define msg \"hello 'world' \\\"quoted\\\"\")\n; comment with 'quotes'".to_string(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
            phantom: false,
            remote_peer: None,
        });
        graph.next_node_id = 2;

        save_graph_to_db(&graph, &db).unwrap();
        let loaded = load_graph_from_db(&db).unwrap().unwrap();
        let n = &loaded.nodes[&1];
        assert_eq!(n.label, "node with 'quotes'");
        assert!(n.script_code.contains("hello 'world'"));
    }

    #[test]
    fn test_multi_canvas() {
        let db = crate::db::Db::new().unwrap();

        // Create two canvases
        let mut g1 = Graph::new();
        g1.nodes.insert(1, Node {
            id: 1, template_name: "Script".to_string(), label: "alpha".to_string(),
            pos: [0.0, 0.0], input_values: HashMap::new(), output_values: HashMap::new(),
            script_code: "(define a 1)".to_string(), script_inputs: Vec::new(),
            script_outputs: Vec::new(), widget_decls: Vec::new(), widget_values: HashMap::new(),
            error: None, last_exec_us: None, render_blocks: Vec::new(),
            phantom: false, remote_peer: None,
        });
        g1.next_node_id = 2;

        let mut g2 = Graph::new();
        g2.nodes.insert(1, Node {
            id: 1, template_name: "Script".to_string(), label: "beta".to_string(),
            pos: [100.0, 100.0], input_values: HashMap::new(), output_values: HashMap::new(),
            script_code: "(define b 2)".to_string(), script_inputs: Vec::new(),
            script_outputs: Vec::new(), widget_decls: Vec::new(), widget_values: HashMap::new(),
            error: None, last_exec_us: None, render_blocks: Vec::new(),
            phantom: false, remote_peer: None,
        });
        g2.next_node_id = 2;

        save_canvas_to_db("project-a", &g1, &db).unwrap();
        save_canvas_to_db("project-b", &g2, &db).unwrap();

        // List canvases
        let mut names = list_canvases(&db);
        names.sort();
        assert_eq!(names, vec!["project-a", "project-b"]);

        // Load each independently
        let la = load_canvas_from_db("project-a", &db).unwrap().unwrap();
        assert_eq!(la.nodes[&1].label, "alpha");

        let lb = load_canvas_from_db("project-b", &db).unwrap().unwrap();
        assert_eq!(lb.nodes[&1].label, "beta");

        // Delete one
        delete_canvas("project-a", &db).unwrap();
        let names = list_canvases(&db);
        assert_eq!(names, vec!["project-b"]);
        assert!(load_canvas_from_db("project-a", &db).unwrap().is_none());
    }
}
