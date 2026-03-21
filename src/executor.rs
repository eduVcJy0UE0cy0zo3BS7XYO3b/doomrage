use crate::db::Db;
use crate::registry::NodeRegistry;
use crate::scheme_engine::SchemeEngine;
use crate::types::*;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use wasmtime::component::{Component, Linker, Val};
use wasmtime::{Config, Engine, Store};

pub struct Executor {
    engine: Engine,
    component_cache: HashMap<String, Arc<Component>>,
    pub scheme: SchemeEngine,
    pub db: Db,
}

impl Executor {
    pub fn new() -> Result<Self> {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true);
        let engine = Engine::new(&cfg)?;
        let scheme = SchemeEngine::new()?;
        let db = Db::new()?;

        // One-time migration: import store.json then rename it
        let store_path = std::path::Path::new("./store.json");
        if store_path.exists() {
            if let Ok(text) = std::fs::read_to_string(store_path) {
                if let Ok(data) = serde_json::from_str::<HashMap<String, serde_json::Value>>(&text)
                {
                    for (key, value) in data {
                        db.kv_set(&key, value);
                    }
                    let _ = std::fs::rename(store_path, "./store.json.migrated");
                    log::info!("Migrated store.json to SurrealDB kv table");
                }
            }
        }

        Ok(Self {
            engine,
            component_cache: HashMap::new(),
            scheme,
            db,
        })
    }

    pub fn invalidate_cache(&mut self, template_name: &str) {
        self.component_cache.remove(template_name);
    }

    /// Execute only the ancestors of `target` + the target itself.
    pub fn execute_up_to(
        &mut self,
        graph: &mut Graph,
        registry: &NodeRegistry,
        target: NodeId,
    ) -> Vec<RunEvent> {
        let order = graph.ancestors_sorted(target);
        self.execute_nodes(graph, registry, &order)
    }

    pub fn execute_graph(
        &mut self,
        graph: &mut Graph,
        registry: &NodeRegistry,
    ) -> Vec<RunEvent> {
        let order = match graph.topological_sort() {
            Ok(order) => order,
            Err(cycle_nodes) => {
                for id in &cycle_nodes {
                    if let Some(node) = graph.nodes.get_mut(id) {
                        node.error = Some("Cycle detected".to_string());
                    }
                }
                return vec![RunEvent {
                    node_id: 0,
                    node_name: "graph".to_string(),
                    duration_us: 0,
                    result: Err("Cycle detected in graph".to_string()),
                }];
            }
        };

        self.execute_nodes(graph, registry, &order)
    }

    fn execute_nodes(
        &mut self,
        graph: &mut Graph,
        registry: &NodeRegistry,
        order: &[NodeId],
    ) -> Vec<RunEvent> {
        let mut events = Vec::new();

        for &node_id in order {
            if let Some(node) = graph.nodes.get_mut(&node_id) {
                node.error = None;
            }
        }

        for &node_id in order {
            let node = match graph.nodes.get(&node_id) {
                Some(n) => n.clone(),
                None => continue,
            };

            let template = match registry.templates.get(&node.template_name) {
                Some(t) => t,
                None => {
                    let err = format!("Template '{}' not found", node.template_name);
                    if let Some(n) = graph.nodes.get_mut(&node_id) {
                        n.error = Some(err.clone());
                    }
                    events.push(RunEvent {
                        node_id,
                        node_name: node.label.clone(),
                        duration_us: 0,
                        result: Err(err),
                    });
                    continue;
                }
            };

            let start = Instant::now();

            let result = if let Some(BuiltinKind::Script) = template.builtin {
                self.execute_script(&node, graph, node_id)
            } else {
                let eff_inputs = node.effective_inputs(Some(template));
                let input_vals = graph.resolve_input_values(node_id, eff_inputs);
                if let Some(BuiltinKind::Const) = template.builtin {
                    self.execute_const(&node, &input_vals)
                } else if let Some(BuiltinKind::Output) = template.builtin {
                    self.execute_output(&input_vals)
                } else {
                    self.execute_wasm_node(template, &input_vals)
                }
            };

            let duration_us = start.elapsed().as_micros() as u64;

            match &result {
                Ok(output_values) => {
                    if let Some(n) = graph.nodes.get_mut(&node_id) {
                        n.output_values = output_values.clone();
                        n.last_exec_us = Some(duration_us);
                    }
                    self.scheme.register_node_library(node_id, output_values);
                    let preview = output_values
                        .iter()
                        .map(|(k, v)| format!("{}: {}", k, v.display()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    events.push(RunEvent {
                        node_id,
                        node_name: node.label.clone(),
                        duration_us,
                        result: Ok(preview),
                    });
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    if let Some(n) = graph.nodes.get_mut(&node_id) {
                        n.error = Some(err_msg.clone());
                        n.last_exec_us = Some(duration_us);
                        for port in &template.outputs {
                            n.output_values
                                .insert(port.name.clone(), port.port_type.default_value());
                        }
                    }
                    events.push(RunEvent {
                        node_id,
                        node_name: node.label.clone(),
                        duration_us,
                        result: Err(err_msg),
                    });
                }
            }
        }

        events
    }

    fn execute_const(
        &self,
        node: &Node,
        _input_vals: &HashMap<String, Value>,
    ) -> Result<HashMap<String, Value>> {
        let mut out = HashMap::new();
        // Const node: output = whatever value is stored in input_values["value"]
        // or the first input_value, or default
        let val = node
            .input_values
            .get("value")
            .cloned()
            .unwrap_or(Value::F64(0.0));
        out.insert("out".to_string(), val);
        Ok(out)
    }

    fn execute_output(
        &self,
        input_vals: &HashMap<String, Value>,
    ) -> Result<HashMap<String, Value>> {
        let mut out = HashMap::new();
        if let Some(val) = input_vals.get("in") {
            out.insert("display".to_string(), val.clone());
        }
        Ok(out)
    }

    fn execute_wasm_node(
        &mut self,
        template: &NodeTemplate,
        input_vals: &HashMap<String, Value>,
    ) -> Result<HashMap<String, Value>> {
        let wasm_bytes = template
            .wasm_bytes
            .as_ref()
            .context("No WASM bytes loaded for this node")?;

        let component = if let Some(cached) = self.component_cache.get(&template.name) {
            cached.clone()
        } else {
            let comp = Arc::new(Component::new(&self.engine, wasm_bytes)?);
            self.component_cache
                .insert(template.name.clone(), comp.clone());
            comp
        };

        let linker: Linker<()> = Linker::new(&self.engine);
        // NOTE: Host imports for canvas-db (store-get, store-set, db-query, etc.)
        // are defined in wit/canvas-db.wit but require WASM components compiled
        // against that WIT to be useful. Current math nodes don't import them.

        let mut store = Store::new(&self.engine, ());

        let instance = linker.instantiate(&mut store, &component)?;

        // Find the "run" export
        let func = instance
            .get_func(&mut store, "run")
            .context("No 'run' export found in WASM component")?;

        // Build params
        let mut params: Vec<Val> = Vec::new();
        for port in &template.inputs {
            let val = input_vals
                .get(&port.name)
                .cloned()
                .unwrap_or_else(|| port.port_type.default_value());
            params.push(value_to_wasm_val(&val));
        }

        // Call
        let mut results = vec![Val::Bool(false); template.outputs.len().max(1)];
        func.call(&mut store, &params, &mut results)?;

        // Parse results
        let mut output_values = HashMap::new();
        for (i, port) in template.outputs.iter().enumerate() {
            if i < results.len() {
                output_values.insert(port.name.clone(), wasm_val_to_value(&results[i]));
            }
        }

        Ok(output_values)
    }

    fn execute_script(
        &self,
        node: &Node,
        graph: &mut Graph,
        node_id: NodeId,
    ) -> Result<HashMap<String, Value>> {
        let code = &node.script_code;
        if code.trim().is_empty() {
            return Ok(HashMap::new());
        }

        let mut available = graph.resolve_all_input_values(node_id);
        for (k, v) in &node.widget_values {
            available.entry(k.clone()).or_insert_with(|| v.clone());
        }

        let script_result = self.scheme.execute_script(&available, Some(&self.db), code)?;

        // Store render blocks and update dynamic ports
        if let Some(n) = graph.nodes.get_mut(&node_id) {
            n.render_blocks = script_result.render_blocks;
            if !script_result.declared_inputs.is_empty() || !script_result.declared_outputs.is_empty() {
                n.script_inputs = script_result.declared_inputs.iter()
                    .map(|(name, type_str)| PortDef {
                        name: name.clone(),
                        port_type: PortType::from_str(type_str).unwrap_or(PortType::F64),
                    })
                    .collect();
                n.script_outputs = script_result.declared_outputs.iter()
                    .map(|(name, type_str)| PortDef {
                        name: name.clone(),
                        port_type: PortType::from_str(type_str).unwrap_or(PortType::F64),
                    })
                    .collect();
            }
            n.widget_decls = script_result.widget_decls;
            // Initialize input_values defaults for new ports
            for port in &n.script_inputs {
                n.input_values.entry(port.name.clone())
                    .or_insert_with(|| port.port_type.default_value());
            }
        }

        Ok(script_result.output_values)
    }

}

fn value_to_wasm_val(val: &Value) -> Val {
    match val {
        Value::F64(v) => Val::Float64(*v),
        Value::F32(v) => Val::Float32(*v),
        Value::I64(v) => Val::S64(*v),
        Value::I32(v) => Val::S32(*v),
        Value::Bool(v) => Val::Bool(*v),
        Value::Str(v) => Val::String(v.clone()),
    }
}

fn wasm_val_to_value(val: &Val) -> Value {
    match val {
        Val::Float64(v) => Value::F64(*v),
        Val::Float32(v) => Value::F32(*v),
        Val::S64(v) => Value::I64(*v),
        Val::S32(v) => Value::I32(*v),
        Val::U64(v) => Value::I64(*v as i64),
        Val::U32(v) => Value::I32(*v as i32),
        Val::Bool(v) => Value::Bool(*v),
        Val::String(v) => Value::Str(v.clone()),
        _ => Value::F64(f64::NAN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::NodeRegistry;

    fn fresh_executor() -> Executor {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true);
        let engine = Engine::new(&cfg).unwrap();
        let scheme = crate::scheme_engine::SchemeEngine::new().unwrap();
        let db = crate::db::Db::new().unwrap();
        Executor {
            engine,
            component_cache: HashMap::new(),
            scheme,
            db,
        }
    }

    fn builtin_registry() -> NodeRegistry {
        let mut reg = NodeRegistry {
            templates: HashMap::new(),
            nodes_dir: std::path::PathBuf::from("/tmp/wasm-canvas-test-nodes"),
        };
        reg.register_builtins();
        reg
    }

    fn const_node(id: NodeId, value: Value) -> Node {
        let mut input_values = HashMap::new();
        input_values.insert("value".to_string(), value);
        Node {
            id,
            template_name: "Const".to_string(),
            label: format!("Const_{}", id),
            pos: [0.0, 0.0],
            input_values,
            output_values: HashMap::new(),
            script_code: String::new(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
        }
    }

    fn output_node(id: NodeId) -> Node {
        Node {
            id,
            template_name: "Output".to_string(),
            label: format!("Output_{}", id),
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
        }
    }

    fn script_node(id: NodeId, code: &str) -> Node {
        Node {
            id,
            template_name: "Script".to_string(),
            label: format!("Script_{}", id),
            pos: [0.0, 0.0],
            input_values: HashMap::new(),
            output_values: HashMap::new(),
            script_code: code.to_string(),
            script_inputs: Vec::new(),
            script_outputs: Vec::new(),
            widget_decls: Vec::new(),
            widget_values: HashMap::new(),
            error: None,
            last_exec_us: None,
            render_blocks: Vec::new(),
        }
    }

    fn make_graph(nodes: Vec<Node>, connections: Vec<(NodeId, &str, NodeId, &str)>) -> Graph {
        let mut graph = Graph::new();
        for node in nodes {
            let id = node.id;
            graph.nodes.insert(id, node);
            if id >= graph.next_node_id {
                graph.next_node_id = id + 1;
            }
        }
        for (from_node, from_port, to_node, to_port) in connections {
            graph.add_connection(from_node, from_port.to_string(), to_node, to_port.to_string());
        }
        graph
    }

    // ── Tier 1: Const and Output ──

    #[test]
    fn test_const_f64() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let mut graph = make_graph(vec![const_node(1, Value::F64(42.0))], vec![]);
        let events = exec.execute_graph(&mut graph, &reg);
        assert_eq!(events.len(), 1);
        assert!(events[0].result.is_ok());
        let out = &graph.nodes[&1].output_values["out"];
        assert!(matches!(out, Value::F64(v) if (*v - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_const_string() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let mut graph = make_graph(vec![const_node(1, Value::Str("hello".into()))], vec![]);
        exec.execute_graph(&mut graph, &reg);
        let out = &graph.nodes[&1].output_values["out"];
        assert!(matches!(out, Value::Str(s) if s == "hello"));
    }

    #[test]
    fn test_output_passthrough() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let mut node = output_node(1);
        node.input_values.insert("in".to_string(), Value::F64(7.0));
        let mut graph = make_graph(vec![node], vec![]);
        exec.execute_graph(&mut graph, &reg);
        let out = &graph.nodes[&1].output_values["display"];
        assert!(matches!(out, Value::F64(v) if (*v - 7.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_missing_template() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let node = Node {
            id: 1,
            template_name: "NonExistent".to_string(),
            label: "Bad".to_string(),
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
        };
        let mut graph = make_graph(vec![node], vec![]);
        let events = exec.execute_graph(&mut graph, &reg);
        assert!(events[0].result.is_err());
        assert!(events[0].result.as_ref().unwrap_err().contains("not found"));
        assert!(graph.nodes[&1].error.is_some());
    }

    // ── Tier 2: DB operations ──

    #[test]
    fn test_db_kv_set() {
        let exec = fresh_executor();
        exec.db.kv_set("test_key", serde_json::json!("test_val"));
        let got = exec.db.kv_get("test_key");
        assert_eq!(got, Some(serde_json::json!("test_val")));
    }

    #[test]
    fn test_db_kv_append() {
        let exec = fresh_executor();
        exec.db.kv_append("arr", serde_json::json!(1));
        exec.db.kv_append("arr", serde_json::json!(2));
        let got = exec.db.kv_get("arr").unwrap();
        assert_eq!(got, serde_json::json!([1, 2]));
    }

    #[test]
    fn test_db_kv_delete() {
        let exec = fresh_executor();
        exec.db.kv_set("gone", serde_json::json!(99));
        exec.db.kv_delete("gone");
        assert!(exec.db.kv_get("gone").is_none());
    }

    #[test]
    fn test_db_run() {
        let exec = fresh_executor();
        exec.db.run("CREATE tasks SET title = 'hello', done = false").unwrap();
        let rows = exec.db.query("SELECT * FROM tasks").unwrap();
        assert!(!rows.is_empty());
        assert_eq!(rows[0]["title"], serde_json::json!("hello"));
    }

    // ── Tier 3: Script execution ──

    #[test]
    fn test_script_arithmetic() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let code = "(define x (input 'x 'f64))\n(define result (output 'result 'f64))\n(set! result (* x 2))";
        let mut node = script_node(1, code);
        node.input_values.insert("x".to_string(), Value::F64(5.0));
        let mut graph = make_graph(vec![node], vec![]);
        exec.execute_graph(&mut graph, &reg);
        let out = &graph.nodes[&1].output_values["result"];
        assert!(matches!(out, Value::F64(v) if (*v - 10.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_script_render_blocks() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let code = "(render (bold \"hi\"))";
        let node = script_node(1, code);
        let mut graph = make_graph(vec![node], vec![]);
        exec.execute_graph(&mut graph, &reg);
        let blocks = &graph.nodes[&1].render_blocks;
        assert!(!blocks.is_empty());
    }

    #[test]
    fn test_script_db_mutation() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let code = "(store-set! \"mykey\" \"myval\")";
        let node = script_node(1, code);
        let mut graph = make_graph(vec![node], vec![]);
        exec.execute_graph(&mut graph, &reg);
        let got = exec.db.kv_get("mykey");
        assert_eq!(got, Some(serde_json::json!("myval")));
    }

    #[test]
    fn test_script_db_query() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        // Pre-seed data
        exec.db.run("CREATE items SET name = 'apple', qty = 3").unwrap();
        let code = "(db-query \"SELECT * FROM items\")";
        let node = script_node(1, code);
        let mut graph = make_graph(vec![node], vec![]);
        let events = exec.execute_graph(&mut graph, &reg);
        assert!(events[0].result.is_ok());
    }

    #[test]
    fn test_script_empty_code() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let mut node = script_node(1, "");
        node.script_code = "   ".to_string(); // whitespace only
        let mut graph = make_graph(vec![node], vec![]);
        let events = exec.execute_graph(&mut graph, &reg);
        assert!(events[0].result.is_ok());
        assert!(graph.nodes[&1].output_values.is_empty());
    }

    // ── Tier 4: Chains ──

    #[test]
    fn test_const_to_output_chain() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let mut graph = make_graph(
            vec![const_node(1, Value::F64(42.0)), output_node(2)],
            vec![(1, "out", 2, "in")],
        );
        exec.execute_graph(&mut graph, &reg);
        let out = &graph.nodes[&2].output_values["display"];
        assert!(matches!(out, Value::F64(v) if (*v - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_const_to_script_chain() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let code = "(define x (input 'x 'f64))\n(define result (output 'result 'f64))\n(set! result (* x 10))";
        let mut graph = make_graph(
            vec![const_node(1, Value::F64(3.0)), script_node(2, code)],
            vec![(1, "out", 2, "x")],
        );
        exec.execute_graph(&mut graph, &reg);
        let out = &graph.nodes[&2].output_values["result"];
        assert!(matches!(out, Value::F64(v) if (*v - 30.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_cycle_detection() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let code_a = "(define x (input 'x 'f64))\n(define result (output 'result 'f64))\n(set! result x)";
        let code_b = "(define x (input 'x 'f64))\n(define result (output 'result 'f64))\n(set! result x)";
        let mut graph = make_graph(
            vec![script_node(1, code_a), script_node(2, code_b)],
            vec![(1, "result", 2, "x"), (2, "result", 1, "x")],
        );
        let events = exec.execute_graph(&mut graph, &reg);
        assert!(events.iter().any(|e| e.result.as_ref().unwrap_err().contains("Cycle")));
    }

    #[test]
    fn test_execute_up_to() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let code = "(define x (input 'x 'f64))\n(define result (output 'result 'f64))\n(set! result (* x 2))";
        // node 3 is independent, should not execute
        let mut graph = make_graph(
            vec![
                const_node(1, Value::F64(5.0)),
                script_node(2, code),
                const_node(3, Value::F64(99.0)),
            ],
            vec![(1, "out", 2, "x")],
        );
        let events = exec.execute_up_to(&mut graph, &reg, 2);
        // Only nodes 1 and 2 should have executed
        let executed_ids: Vec<NodeId> = events.iter().map(|e| e.node_id).collect();
        assert!(executed_ids.contains(&1));
        assert!(executed_ids.contains(&2));
        assert!(!executed_ids.contains(&3));
        let out = &graph.nodes[&2].output_values["result"];
        assert!(matches!(out, Value::F64(v) if (*v - 10.0).abs() < f64::EPSILON));
    }

    // ── Tier 5: DB roundtrip ──

    #[test]
    fn test_script_creates_then_queries() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let create_code = "(db-run \"CREATE items SET name = 'banana', qty = 5\")";
        let query_code = "(db-query \"SELECT * FROM items\")";
        let mut graph = make_graph(
            vec![script_node(1, create_code), script_node(2, query_code)],
            // Connection to enforce ordering: node 1 before node 2
            // We use a dummy port — but since there's no declared port, let's rely on topo order.
            // Nodes with lower IDs come first in topo sort (sorted queue).
            vec![],
        );
        exec.execute_graph(&mut graph, &reg);
        // Both should succeed
        assert!(graph.nodes[&1].error.is_none());
        assert!(graph.nodes[&2].error.is_none());
        // Verify data actually exists
        let rows = exec.db.query("SELECT * FROM items").unwrap();
        assert!(!rows.is_empty());
    }

    #[test]
    fn test_script_store_set_then_read() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        // Script 1 sets a kv value
        let set_code = "(store-set! \"color\" \"red\")";
        // Script 2 reads it back via store-get (injected at script start)
        // Use db-query as a reliable fallback to verify the value landed in DB
        let read_code = "(db-query \"SELECT * FROM kv WHERE key = 'color'\")";
        let mut graph = make_graph(
            vec![script_node(1, set_code), script_node(2, read_code)],
            vec![],
        );
        exec.execute_graph(&mut graph, &reg);
        assert!(graph.nodes[&1].error.is_none());
        assert!(graph.nodes[&2].error.is_none());
        // Verify via direct DB access
        let got = exec.db.kv_get("color");
        assert_eq!(got, Some(serde_json::json!("red")));
    }

    // ── Tier 6: Node as R6RS module ──

    #[test]
    fn test_node_library_registration() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let mut graph = make_graph(vec![const_node(1, Value::F64(42.0))], vec![]);
        exec.execute_graph(&mut graph, &reg);
        // After execution, node 1's output should be registered as (node 1)
        let env = exec.scheme.make_env();
        env.eval(true, "(import (node n1))").unwrap();
        let results = env.eval(false, "out").unwrap();
        let val = results[0].cast_to_scheme_type::<f64>().unwrap();
        assert!((val - 42.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_chain_via_import() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();

        // Const(42) → Script that imports (node n1) via R6RS module.
        // We need a connection to ensure topo order (1 before 2).
        let code = "(import (node n1))\n(define result (output 'result 'f64))\n(set! result (* out 2))";
        let mut graph = make_graph(
            vec![const_node(1, Value::F64(42.0)), script_node(2, code)],
            // Dummy connection to enforce ordering
            vec![(1, "out", 2, "x")],
        );
        exec.execute_graph(&mut graph, &reg);
        assert!(graph.nodes[&2].error.is_none(), "Script error: {:?}", graph.nodes[&2].error);
        let out = &graph.nodes[&2].output_values["result"];
        assert!(matches!(out, Value::F64(v) if (*v - 84.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_new_syntax_with_connections() {
        let mut exec = fresh_executor();
        let reg = builtin_registry();
        let code = "(define x (input 'x 'f64))\n(define result (output 'result 'f64))\n(set! result (* x 3))";
        let mut graph = make_graph(
            vec![const_node(1, Value::F64(10.0)), script_node(2, code)],
            vec![(1, "out", 2, "x")],
        );
        exec.execute_graph(&mut graph, &reg);
        let out = &graph.nodes[&2].output_values["result"];
        assert!(matches!(out, Value::F64(v) if (*v - 30.0).abs() < f64::EPSILON));
    }
}
