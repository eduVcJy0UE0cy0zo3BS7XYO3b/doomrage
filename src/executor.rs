use crate::db::Db;
use crate::scheme_engine::SchemeEngine;
use crate::types::*;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wasmtime::component::{Component, Linker, Val};
use wasmtime::{Config, Engine, Store};

/// Thread-safe WASM component runner with compilation cache.
#[derive(Clone)]
pub struct WasmRunner {
    engine: Engine,
    component_cache: Arc<Mutex<HashMap<String, Arc<Component>>>>,
}

impl WasmRunner {
    pub fn new() -> Result<Self> {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true);
        let engine = Engine::new(&cfg)?;
        Ok(Self {
            engine,
            component_cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Execute a WASM node given its template and input values.
    pub fn execute(
        &self,
        template: &NodeTemplate,
        input_vals: &HashMap<String, Value>,
    ) -> Result<HashMap<String, Value>> {
        let wasm_bytes = template
            .wasm_bytes
            .as_ref()
            .context("No WASM bytes loaded for this node")?;

        let component = {
            let mut cache = self.component_cache.lock().unwrap();
            if let Some(cached) = cache.get(&template.name) {
                cached.clone()
            } else {
                let comp = Arc::new(Component::new(&self.engine, wasm_bytes)?);
                cache.insert(template.name.clone(), comp.clone());
                comp
            }
        };

        let linker: Linker<()> = Linker::new(&self.engine);
        let mut store = Store::new(&self.engine, ());
        let instance = linker.instantiate(&mut store, &component)?;

        let func = instance
            .get_func(&mut store, "run")
            .context("No 'run' export found in WASM component")?;

        let mut params: Vec<Val> = Vec::new();
        for port in &template.inputs {
            let val = input_vals
                .get(&port.name)
                .cloned()
                .unwrap_or_else(|| port.port_type.default_value());
            params.push(value_to_wasm_val(&val));
        }

        let mut results = vec![Val::Bool(false); template.outputs.len().max(1)];
        func.call(&mut store, &params, &mut results)?;

        let mut output_values = HashMap::new();
        for (i, port) in template.outputs.iter().enumerate() {
            if i < results.len() {
                output_values.insert(port.name.clone(), wasm_val_to_value(&results[i]));
            }
        }

        Ok(output_values)
    }
}

/// Execute a Const builtin node.
pub fn execute_const(node: &Node) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let val = node
        .input_values
        .get("value")
        .cloned()
        .unwrap_or(Value::F64(0.0));
    out.insert("out".to_string(), val);
    out
}

/// Execute an Output builtin node.
pub fn execute_output(input_vals: &HashMap<String, Value>) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    if let Some(val) = input_vals.get("in") {
        out.insert("display".to_string(), val.clone());
    }
    out
}

/// App-level holder for shared resources created at startup.
pub struct AppResources {
    pub scheme: Arc<SchemeEngine>,
    pub db: Db,
    pub wasm: WasmRunner,
}

impl AppResources {
    pub fn new() -> Result<Self> {
        let scheme = Arc::new(SchemeEngine::new()?);
        let db = Db::new()?;
        let wasm = WasmRunner::new()?;

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

        Ok(Self { scheme, db, wasm })
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
    use crate::actor::{ActorResult, ActorRuntime};
    use crate::registry::NodeRegistry;

    fn fresh_resources() -> AppResources {
        AppResources::new().unwrap()
    }

    fn builtin_registry() -> NodeRegistry {
        let mut reg = NodeRegistry {
            templates: HashMap::new(),
            nodes_dir: std::env::temp_dir().join("wasm-canvas-test-nodes"),
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

    fn make_graph(nodes: Vec<Node>) -> Graph {
        let mut graph = Graph::new();
        for node in nodes {
            let id = node.id;
            graph.nodes.insert(id, node);
            if id >= graph.next_node_id {
                graph.next_node_id = id + 1;
            }
        }
        graph
    }

    // ── Const and Output ──

    #[test]
    fn test_const_f64() {
        let node = const_node(1, Value::F64(42.0));
        let out = execute_const(&node);
        assert!(matches!(out.get("out"), Some(Value::F64(v)) if (*v - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_const_string() {
        let node = const_node(1, Value::Str("hello".into()));
        let out = execute_const(&node);
        assert!(matches!(out.get("out"), Some(Value::Str(s)) if s == "hello"));
    }

    #[test]
    fn test_output_passthrough() {
        let mut input_vals = HashMap::new();
        input_vals.insert("in".to_string(), Value::F64(7.0));
        let out = execute_output(&input_vals);
        assert!(matches!(out.get("display"), Some(Value::F64(v)) if (*v - 7.0).abs() < f64::EPSILON));
    }

    // ── DB operations ──

    #[test]
    fn test_db_kv_set() {
        let res = fresh_resources();
        res.db.kv_set("test_key", serde_json::json!("test_val"));
        let got = res.db.kv_get("test_key");
        assert_eq!(got, Some(serde_json::json!("test_val")));
    }

    #[test]
    fn test_db_kv_append() {
        let res = fresh_resources();
        res.db.kv_append("arr", serde_json::json!(1));
        res.db.kv_append("arr", serde_json::json!(2));
        let got = res.db.kv_get("arr").unwrap();
        assert_eq!(got, serde_json::json!([1, 2]));
    }

    #[test]
    fn test_db_kv_delete() {
        let res = fresh_resources();
        res.db.kv_set("gone", serde_json::json!(99));
        res.db.kv_delete("gone");
        assert!(res.db.kv_get("gone").is_none());
    }

    #[test]
    fn test_db_run() {
        let res = fresh_resources();
        res.db.run("CREATE tasks SET title = 'hello', done = false").unwrap();
        let rows = res.db.query("SELECT * FROM tasks").unwrap();
        assert!(!rows.is_empty());
        assert_eq!(rows[0]["title"], serde_json::json!("hello"));
    }

    fn send_script(rt: &mut ActorRuntime, code: &str, inputs: HashMap<String, Value>, db: &Db) {
        rt.compute(
            1,
            script_node(1, code),
            Some(builtin_registry().templates["Script"].clone()),
            inputs,
            HashMap::new(),
            db.clone(),
        );
    }

    // ── Script execution via ActorRuntime ──

    #[test]
    fn test_script_arithmetic() {

        let res = fresh_resources();
        let mut rt = ActorRuntime::with_debounce(Arc::clone(&res.scheme), 0);
        let code = "(define x 5)\n(define result (* x 2))";
        let mut node = script_node(1, code);
        node.exports = vec!["result".to_string()];
        rt.compute(
            1,
            node,
            Some(builtin_registry().templates["Script"].clone()),
            HashMap::new(),
            HashMap::new(),
            res.db.clone(),
        );
        // Wait for result
        loop {
            if let Some(result) = rt.poll() {
                match result {
                    ActorResult::Computed { result, .. } => {
                        let out = &result.output_values["result"];
                        assert!(matches!(out, Value::F64(v) if (*v - 10.0).abs() < f64::EPSILON));
                        return;
                    }
                    ActorResult::Error { message, .. } => panic!("Script error: {}", message),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn test_script_render_blocks() {

        let res = fresh_resources();
        let mut rt = ActorRuntime::with_debounce(Arc::clone(&res.scheme), 0);
        send_script(&mut rt,"(render (bold \"hi\"))", HashMap::new(), &res.db);
        loop {
            if let Some(result) = rt.poll() {
                match result {
                    ActorResult::Computed { result, .. } => {
                        assert!(!result.render_blocks.is_empty());
                        return;
                    }
                    ActorResult::Error { message, .. } => panic!("Script error: {}", message),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn test_script_db_mutation() {

        let res = fresh_resources();
        let mut rt = ActorRuntime::with_debounce(Arc::clone(&res.scheme), 0);
        send_script(&mut rt,"(store-set! \"mykey\" \"myval\")", HashMap::new(), &res.db);
        loop {
            if let Some(result) = rt.poll() {
                match result {
                    ActorResult::Computed { .. } => {
                        let got = res.db.kv_get("mykey");
                        assert_eq!(got, Some(serde_json::json!("myval")));
                        return;
                    }
                    ActorResult::Error { message, .. } => panic!("Script error: {}", message),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn test_script_db_query() {

        let res = fresh_resources();
        res.db.run("CREATE items SET name = 'apple', qty = 3").unwrap();
        let mut rt = ActorRuntime::with_debounce(Arc::clone(&res.scheme), 0);
        send_script(&mut rt,"(db-query \"SELECT * FROM items\")", HashMap::new(), &res.db);
        loop {
            if let Some(result) = rt.poll() {
                match result {
                    ActorResult::Computed { .. } => return,
                    ActorResult::Error { message, .. } => panic!("Script error: {}", message),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn test_script_empty_code() {

        let res = fresh_resources();
        let mut rt = ActorRuntime::with_debounce(Arc::clone(&res.scheme), 0);
        send_script(&mut rt,"   ", HashMap::new(), &res.db);
        loop {
            if let Some(result) = rt.poll() {
                match result {
                    ActorResult::Computed { result, .. } => {
                        assert!(result.output_values.is_empty());
                        return;
                    }
                    ActorResult::Error { message, .. } => panic!("Script error: {}", message),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    // ── Node as R6RS module ──

    #[test]
    fn test_node_library_registration() {
        let res = fresh_resources();
        let mut outputs = HashMap::new();
        outputs.insert("out".to_string(), Value::F64(42.0));
        res.scheme.register_node_library_named(1, "test-canvas", "my-mod", &outputs);
        let env = res.scheme.make_env();
        env.eval(true, "(import (test-canvas my-mod))").unwrap();
        let results = env.eval(false, "out").unwrap();
        let val = results[0].cast_to_scheme_type::<f64>().unwrap();
        assert!((val - 42.0).abs() < f64::EPSILON);
    }

    // ── DB roundtrip ──

    #[test]
    fn test_script_store_set_then_read() {

        let res = fresh_resources();
        let mut rt = ActorRuntime::with_debounce(Arc::clone(&res.scheme), 0);

        // Script sets a kv value
        send_script(&mut rt,"(store-set! \"color\" \"red\")", HashMap::new(), &res.db);
        loop {
            if let Some(result) = rt.poll() {
                match result {
                    ActorResult::Computed { .. } => break,
                    ActorResult::Error { message, .. } => panic!("{}", message),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let got = res.db.kv_get("color");
        assert_eq!(got, Some(serde_json::json!("red")));
    }
}
