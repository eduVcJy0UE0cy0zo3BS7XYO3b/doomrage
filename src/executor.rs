use crate::registry::NodeRegistry;
use crate::scheme_engine::SchemeEngine;
use crate::store::Store as AppStore;
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
    pub store: AppStore,
}

impl Executor {
    pub fn new() -> Result<Self> {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true);
        let engine = Engine::new(&cfg)?;
        let scheme = SchemeEngine::new()?;
        let store = AppStore::load(std::path::Path::new("./store.json"))?;
        Ok(Self {
            engine,
            component_cache: HashMap::new(),
            scheme,
            store,
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

            let mut input_vals: HashMap<String, Value> = node.input_values.clone();
            let eff_inputs = node.effective_inputs(Some(template));
            for port in eff_inputs {
                if let Some(conn) = graph.input_connection(node_id, &port.name) {
                    if let Some(src_node) = graph.nodes.get(&conn.from_node) {
                        if let Some(val) = src_node.output_values.get(&conn.from_port) {
                            input_vals.insert(port.name.clone(), val.clone());
                        }
                    }
                }
            }

            let result = if let Some(BuiltinKind::Const) = template.builtin {
                self.execute_const(&node, &input_vals)
            } else if let Some(BuiltinKind::Output) = template.builtin {
                self.execute_output(&input_vals)
            } else if let Some(BuiltinKind::Script) = template.builtin {
                self.execute_script(&node, template, &input_vals, graph, node_id)
            } else {
                self.execute_wasm_node(template, &input_vals)
            };

            let duration_us = start.elapsed().as_micros() as u64;

            match &result {
                Ok(output_values) => {
                    if let Some(n) = graph.nodes.get_mut(&node_id) {
                        n.output_values = output_values.clone();
                        n.last_exec_us = Some(duration_us);
                    }
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
        _template: &NodeTemplate,
        input_vals: &HashMap<String, Value>,
        graph: &mut Graph,
        node_id: NodeId,
    ) -> Result<HashMap<String, Value>> {
        use crate::scheme_engine::{parse_port_declarations, ScriptValue};

        let code = &node.script_code;
        if code.trim().is_empty() {
            return Ok(HashMap::new());
        }

        let (input_decls, output_decls) = parse_port_declarations(code);

        // Build bindings from declared inputs
        let bindings: Vec<(String, f64)> = input_decls
            .iter()
            .filter_map(|decl| {
                let val = input_vals.get(&decl.name)?;
                let f = match val {
                    Value::F64(v) => *v,
                    Value::F32(v) => *v as f64,
                    Value::I64(v) => *v as f64,
                    Value::I32(v) => *v as f64,
                    Value::Bool(v) => if *v { 1.0 } else { 0.0 },
                    _ => return None,
                };
                Some((decl.name.clone(), f))
            })
            .collect();

        let output_names: Vec<String> = output_decls.iter().map(|d| d.name.clone()).collect();

        let script_result = self.scheme.execute_script(&bindings, &output_names, Some(&self.store), code)?;

        // Process store mutations
        for mutation in &script_result.store_mutations {
            self.apply_store_mutation(mutation);
        }

        // Store render blocks
        if let Some(n) = graph.nodes.get_mut(&node_id) {
            n.render_blocks = script_result.render_blocks;
        }

        // Convert outputs
        let mut output_values = HashMap::new();
        for (name, val) in &script_result.output_values {
            match val {
                ScriptValue::Number(f) => {
                    output_values.insert(name.clone(), Value::F64(*f));
                }
                ScriptValue::Str(s) => {
                    output_values.insert(name.clone(), Value::Str(s.clone()));
                }
            }
        }

        Ok(output_values)
    }

    pub fn apply_store_mutation_pub(&self, mutation: &str) {
        self.apply_store_mutation(mutation);
    }

    fn apply_store_mutation(&self, mutation: &str) {
        // Parse "(store-set key value)" etc.
        let inner = mutation.trim().strip_prefix('(').and_then(|s| s.strip_suffix(')'));
        let inner = match inner {
            Some(s) => s,
            None => return,
        };

        let parts: Vec<&str> = inner.splitn(3, ' ').collect();
        match parts.first().copied() {
            Some("store-set") if parts.len() >= 3 => {
                let key = parts[1].trim_matches('"');
                let val_str = parts[2].trim_matches('"');
                self.store.set(key, AppStore::scheme_to_value(val_str));
                let _ = self.store.save();
            }
            Some("store-append") if parts.len() >= 3 => {
                let key = parts[1].trim_matches('"');
                let val_str = parts[2].trim_matches('"');
                self.store.append(key, AppStore::scheme_to_value(val_str));
                let _ = self.store.save();
            }
            Some("store-delete") if parts.len() >= 2 => {
                let key = parts[1].trim_matches('"');
                self.store.delete(key);
                let _ = self.store.save();
            }
            _ => {}
        }
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
