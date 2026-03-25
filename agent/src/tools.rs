use nrepl::bencode::Value as BValue;
use nrepl::Client;
use serde_json::{json, Value};

/// Build the tools array for Claude API.
pub fn tools_schema() -> Value {
    json!([
        {
            "name": "list_canvases",
            "description": "List all canvases in the project",
            "input_schema": {"type": "object", "properties": {}}
        },
        {
            "name": "create_canvas",
            "description": "Create a new canvas",
            "input_schema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }
        },
        {
            "name": "list_nodes",
            "description": "List all nodes on all canvases with their labels and exports",
            "input_schema": {"type": "object", "properties": {}}
        },
        {
            "name": "create_node",
            "description": "Create a new Script node on a canvas",
            "input_schema": {
                "type": "object",
                "properties": {
                    "canvas": {"type": "string", "description": "Canvas name"},
                    "label": {"type": "string", "description": "Node label (becomes module name for imports)"},
                    "code": {"type": "string", "description": "Scheme source code"},
                    "exports": {"type": "array", "items": {"type": "string"}, "description": "Variable names to export"},
                    "imports": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}, "description": "Dependencies as [canvas, module] pairs"}
                },
                "required": ["canvas", "label", "code"]
            }
        },
        {
            "name": "update_node",
            "description": "Update a node's code, exports, or imports",
            "input_schema": {
                "type": "object",
                "properties": {
                    "canvas": {"type": "string"},
                    "label": {"type": "string"},
                    "code": {"type": "string", "description": "New Scheme code (optional)"},
                    "exports": {"type": "array", "items": {"type": "string"}, "description": "New exports (optional)"},
                    "imports": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}, "description": "New imports (optional)"}
                },
                "required": ["canvas", "label"]
            }
        },
        {
            "name": "delete_node",
            "description": "Delete a node from a canvas",
            "input_schema": {
                "type": "object",
                "properties": {
                    "canvas": {"type": "string"},
                    "label": {"type": "string"}
                },
                "required": ["canvas", "label"]
            }
        },
        {
            "name": "compute_node",
            "description": "Trigger computation of a node. Check results with node_state.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "canvas": {"type": "string"},
                    "label": {"type": "string"}
                },
                "required": ["canvas", "label"]
            }
        },
        {
            "name": "node_state",
            "description": "Read a node's current state: code, exports, imports, outputs, and error",
            "input_schema": {
                "type": "object",
                "properties": {
                    "canvas": {"type": "string"},
                    "label": {"type": "string"}
                },
                "required": ["canvas", "label"]
            }
        },
        {
            "name": "eval_scheme",
            "description": "Evaluate Scheme code in the REPL. Good for testing expressions.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "Scheme code to evaluate"}
                },
                "required": ["code"]
            }
        }
    ])
}

/// Execute a tool call by sending the corresponding nREPL op.
pub fn execute_tool(client: &mut Client, session: &str, name: &str, input: &Value) -> String {
    match name {
        "list_canvases" => {
            let msg = BValue::dict(vec![
                ("id", BValue::string("tool")),
                ("op", BValue::string("list-canvases")),
                ("session", BValue::string(session)),
            ]);
            send_and_format(client, &msg)
        }
        "create_canvas" => {
            let canvas = input["name"].as_str().unwrap_or("");
            let msg = BValue::dict(vec![
                ("id", BValue::string("tool")),
                ("name", BValue::string(canvas)),
                ("op", BValue::string("create-canvas")),
                ("session", BValue::string(session)),
            ]);
            send_and_format(client, &msg)
        }
        "list_nodes" => {
            let msg = BValue::dict(vec![
                ("id", BValue::string("tool")),
                ("op", BValue::string("ns-list")),
                ("session", BValue::string(session)),
            ]);
            send_and_format(client, &msg)
        }
        "create_node" => {
            let canvas = input["canvas"].as_str().unwrap_or("");
            let label = input["label"].as_str().unwrap_or("");
            let code = input["code"].as_str().unwrap_or("");
            let exports = input.get("exports")
                .and_then(|v| v.as_array())
                .map(|arr| BValue::List(arr.iter().filter_map(|v| v.as_str().map(BValue::string)).collect()))
                .unwrap_or(BValue::List(vec![]));
            let imports = input.get("imports")
                .and_then(|v| v.as_array())
                .map(|arr| BValue::List(arr.iter().filter_map(|v| {
                    let pair = v.as_array()?;
                    if pair.len() == 2 {
                        Some(BValue::List(vec![
                            BValue::string(pair[0].as_str()?),
                            BValue::string(pair[1].as_str()?),
                        ]))
                    } else { None }
                }).collect()))
                .unwrap_or(BValue::List(vec![]));
            let msg = BValue::dict(vec![
                ("canvas", BValue::string(canvas)),
                ("code", BValue::string(code)),
                ("exports", exports),
                ("id", BValue::string("tool")),
                ("imports", imports),
                ("label", BValue::string(label)),
                ("op", BValue::string("create-node")),
                ("session", BValue::string(session)),
            ]);
            send_and_format(client, &msg)
        }
        "update_node" => {
            let canvas = input["canvas"].as_str().unwrap_or("");
            let label = input["label"].as_str().unwrap_or("");
            let mut pairs = vec![
                ("canvas", BValue::string(canvas)),
                ("id", BValue::string("tool")),
                ("label", BValue::string(label)),
                ("op", BValue::string("update-node")),
                ("session", BValue::string(session)),
            ];
            if let Some(code) = input.get("code").and_then(|v| v.as_str()) {
                pairs.push(("code", BValue::string(code)));
            }
            if let Some(exports) = input.get("exports").and_then(|v| v.as_array()) {
                pairs.push(("exports", BValue::List(
                    exports.iter().filter_map(|v| v.as_str().map(BValue::string)).collect()
                )));
            }
            if let Some(imports) = input.get("imports").and_then(|v| v.as_array()) {
                pairs.push(("imports", BValue::List(
                    imports.iter().filter_map(|v| {
                        let pair = v.as_array()?;
                        if pair.len() == 2 {
                            Some(BValue::List(vec![
                                BValue::string(pair[0].as_str()?),
                                BValue::string(pair[1].as_str()?),
                            ]))
                        } else { None }
                    }).collect()
                )));
            }
            let msg = BValue::dict(pairs);
            send_and_format(client, &msg)
        }
        "delete_node" => {
            let canvas = input["canvas"].as_str().unwrap_or("");
            let label = input["label"].as_str().unwrap_or("");
            let msg = BValue::dict(vec![
                ("canvas", BValue::string(canvas)),
                ("id", BValue::string("tool")),
                ("label", BValue::string(label)),
                ("op", BValue::string("delete-node")),
                ("session", BValue::string(session)),
            ]);
            send_and_format(client, &msg)
        }
        "compute_node" => {
            let canvas = input["canvas"].as_str().unwrap_or("");
            let label = input["label"].as_str().unwrap_or("");
            let msg = BValue::dict(vec![
                ("canvas", BValue::string(canvas)),
                ("id", BValue::string("tool")),
                ("label", BValue::string(label)),
                ("op", BValue::string("compute")),
                ("session", BValue::string(session)),
            ]);
            send_and_format(client, &msg)
        }
        "node_state" => {
            let canvas = input["canvas"].as_str().unwrap_or("");
            let label = input["label"].as_str().unwrap_or("");
            let msg = BValue::dict(vec![
                ("canvas", BValue::string(canvas)),
                ("id", BValue::string("tool")),
                ("label", BValue::string(label)),
                ("op", BValue::string("node-state")),
                ("session", BValue::string(session)),
            ]);
            send_and_format(client, &msg)
        }
        "eval_scheme" => {
            let code = input["code"].as_str().unwrap_or("");
            match client.eval(session, code) {
                Ok(responses) => {
                    let mut result = String::new();
                    for resp in &responses {
                        if let Some(val) = resp.get_str("value") {
                            result.push_str(val);
                            result.push('\n');
                        }
                        if let Some(ex) = resp.get_str("ex") {
                            result.push_str(&format!("Error: {}\n", ex));
                        }
                    }
                    if result.is_empty() { "ok".into() } else { result.trim().into() }
                }
                Err(e) => format!("Error: {}", e),
            }
        }
        _ => format!("Unknown tool: {}", name),
    }
}

fn send_and_format(client: &mut Client, msg: &BValue) -> String {
    if let Err(e) = client.send(msg) {
        return format!("Error sending: {}", e);
    }
    match client.recv_until_done() {
        Ok(responses) => {
            let last = match responses.last() {
                Some(r) => r,
                None => return "No response".into(),
            };
            format!("{}", last)
        }
        Err(e) => format!("Error receiving: {}", e),
    }
}
