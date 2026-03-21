use crate::db::Db;
use crate::registry::NodeRegistry;
use crate::types::{Graph, NodeId};
use scheme_rs::exceptions::Exception;
use scheme_rs::registry::bridge;
use scheme_rs::value::Value;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Shared network values store: (peer, channel) -> (key -> Value)
pub type NetValues = Arc<Mutex<HashMap<(String, String), HashMap<String, crate::types::Value>>>>;

thread_local! {
    static THREAD_DB: RefCell<Option<Db>> = RefCell::new(None);
}

// --- Port registry (dynamic port discovery) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetDecl {
    pub name: String,
    pub widget_type: String,
    pub params: Vec<f64>,
}

pub struct PortRegistry {
    pub inputs: Vec<(String, String)>,
    pub outputs: Vec<(String, String)>,
    pub widgets: Vec<WidgetDecl>,
}

impl PortRegistry {
    pub fn new() -> Self {
        Self {
            inputs: Vec::new(),
            outputs: Vec::new(),
            widgets: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty() && self.widgets.is_empty()
    }
}

thread_local! {
    static THREAD_INPUTS: RefCell<Option<HashMap<String, crate::types::Value>>> = RefCell::new(None);
    static THREAD_PORTS: RefCell<PortRegistry> = RefCell::new(PortRegistry::new());
    static THREAD_NET_VALUES: RefCell<Option<NetValues>> = RefCell::new(None);
    static THREAD_NET_PUBLISH: RefCell<Vec<String>> = RefCell::new(Vec::new());
}

/// Set thread-local net values store for bridge functions.
pub fn set_thread_net_values(nv: Option<&NetValues>) {
    THREAD_NET_VALUES.with(|cell| {
        *cell.borrow_mut() = nv.cloned();
    });
}

/// Take collected net-publish channel names from thread-local.
pub fn take_net_publishes() -> Vec<String> {
    THREAD_NET_PUBLISH.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

/// Scope-guard: sets thread-local input values and clears port registry before `f`,
/// returns collected PortRegistry after.
pub fn with_port_context<R>(
    available_inputs: Option<&HashMap<String, crate::types::Value>>,
    f: impl FnOnce() -> R,
) -> (R, PortRegistry) {
    THREAD_INPUTS.with(|cell| {
        *cell.borrow_mut() = available_inputs.cloned();
    });
    THREAD_PORTS.with(|cell| {
        *cell.borrow_mut() = PortRegistry::new();
    });

    let result = f();

    THREAD_INPUTS.with(|cell| {
        cell.borrow_mut().take();
    });
    let registry = THREAD_PORTS.with(|cell| {
        std::mem::replace(&mut *cell.borrow_mut(), PortRegistry::new())
    });

    (result, registry)
}

// --- Graph context (Phase 6) ---

pub struct GraphContext {
    graph: *mut Graph,
    registry: *const NodeRegistry,
}

// SAFETY: GraphContext is only used within with_graph_context scope-guard
unsafe impl Send for GraphContext {}
unsafe impl Sync for GraphContext {}

thread_local! {
    static THREAD_GRAPH: RefCell<Option<GraphContext>> = RefCell::new(None);
}

pub fn with_graph_context<R>(
    graph: &mut Graph,
    registry: &NodeRegistry,
    f: impl FnOnce() -> R,
) -> R {
    THREAD_GRAPH.with(|cell| {
        cell.borrow_mut().replace(GraphContext {
            graph: graph as *mut Graph,
            registry: registry as *const NodeRegistry,
        });
    });
    let result = f();
    THREAD_GRAPH.with(|cell| {
        cell.borrow_mut().take();
    });
    result
}

fn with_graph<R>(f: impl FnOnce(&mut Graph, &NodeRegistry) -> R) -> Result<R, Exception> {
    THREAD_GRAPH.with(|cell| {
        let borrow = cell.borrow();
        match borrow.as_ref() {
            Some(ctx) => {
                // SAFETY: pointer is valid within with_graph_context scope
                let graph = unsafe { &mut *ctx.graph };
                let registry = unsafe { &*ctx.registry };
                Ok(f(graph, registry))
            }
            None => Err(Exception::error("No graph context available")),
        }
    })
}

/// Scope-guard: sets thread-local Db before `f`, clears after.
pub fn with_db_context<R>(db: &Db, f: impl FnOnce() -> R) -> R {
    THREAD_DB.with(|cell| {
        cell.borrow_mut().replace(db.clone());
    });
    let result = f();
    THREAD_DB.with(|cell| {
        cell.borrow_mut().take();
    });
    result
}

fn with_db<R>(f: impl FnOnce(&Db) -> R) -> Result<R, Exception> {
    THREAD_DB.with(|cell| {
        let borrow = cell.borrow();
        match borrow.as_ref() {
            Some(db) => Ok(f(db)),
            None => Err(Exception::error("No database context available")),
        }
    })
}

fn json_to_scheme_value(val: &serde_json::Value) -> Value {
    match val {
        serde_json::Value::Null => Value::null(),
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                Value::from(f)
            } else {
                Value::from(format!("{}", n))
            }
        }
        serde_json::Value::String(s) => Value::from(s.clone()),
        serde_json::Value::Array(arr) => {
            let mut list = Value::null();
            for item in arr.iter().rev() {
                list = Value::from(scheme_rs::lists::Pair::new(
                    json_to_scheme_value(item),
                    list,
                    true,
                ));
            }
            list
        }
        serde_json::Value::Object(obj) => {
            // (json-object (key . val) ...)
            let tag = Value::from(scheme_rs::symbols::Symbol::intern("json-object"));
            let mut list = Value::null();
            for (k, v) in obj.iter().rev() {
                let pair = Value::from(scheme_rs::lists::Pair::new(
                    Value::from(k.clone()),
                    json_to_scheme_value(v),
                    false,
                ));
                list = Value::from(scheme_rs::lists::Pair::new(pair, list, true));
            }
            Value::from(scheme_rs::lists::Pair::new(tag, list, true))
        }
    }
}

fn value_to_string(val: &Value) -> String {
    format!("{}", val)
}

#[bridge(name = "store-get", lib = "(canvas db)")]
fn bridge_store_get(key: &Value) -> Result<Vec<Value>, Exception> {
    let key_str = value_to_string(key);
    with_db(|db| {
        match db.kv_get(&key_str) {
            Some(json_val) => json_to_scheme_value(&json_val),
            None => Value::from(String::new()),
        }
    })
    .map(|v| vec![v])
}

#[bridge(name = "store-set!", lib = "(canvas db)")]
fn bridge_store_set(key: &Value, value: &Value) -> Result<Vec<Value>, Exception> {
    let key_str = value_to_string(key);
    let json_val = crate::scheme_engine::scheme_value_to_json(value);
    with_db(|db| {
        db.kv_set(&key_str, json_val);
    })?;
    Ok(vec![Value::null()])
}

#[bridge(name = "store-delete!", lib = "(canvas db)")]
fn bridge_store_delete(key: &Value) -> Result<Vec<Value>, Exception> {
    let key_str = value_to_string(key);
    with_db(|db| {
        db.kv_delete(&key_str);
    })?;
    Ok(vec![Value::null()])
}

#[bridge(name = "store-append!", lib = "(canvas db)")]
fn bridge_store_append(key: &Value, value: &Value) -> Result<Vec<Value>, Exception> {
    let key_str = value_to_string(key);
    let json_val = crate::scheme_engine::scheme_value_to_json(value);
    with_db(|db| {
        db.kv_append(&key_str, json_val);
    })?;
    Ok(vec![Value::null()])
}

#[bridge(name = "store-keys", lib = "(canvas db)")]
fn bridge_store_keys() -> Result<Vec<Value>, Exception> {
    with_db(|db| {
        let keys = db.kv_keys();
        let mut list = Value::null();
        for key in keys.into_iter().rev() {
            list = Value::from(scheme_rs::lists::Pair::new(
                Value::from(key),
                list,
                true,
            ));
        }
        list
    })
    .map(|v| vec![v])
}

#[bridge(name = "db-query", lib = "(canvas db)")]
fn bridge_db_query(surql: &Value) -> Result<Vec<Value>, Exception> {
    let query_str = value_to_string(surql);
    with_db(|db| {
        match db.query(&query_str) {
            Ok(results) => {
                let mut list = Value::null();
                for row in results.iter().rev() {
                    list = Value::from(scheme_rs::lists::Pair::new(
                        json_to_scheme_value(row),
                        list,
                        true,
                    ));
                }
                list
            }
            Err(e) => {
                // Return error as a list with error message
                let err_msg = Value::from(format!("db-query error: {}", e));
                Value::from(scheme_rs::lists::Pair::new(err_msg, Value::null(), true))
            }
        }
    })
    .map(|v| vec![v])
}

#[bridge(name = "db-run", lib = "(canvas db)")]
fn bridge_db_run(surql: &Value) -> Result<Vec<Value>, Exception> {
    let query_str = value_to_string(surql);
    with_db(|db| {
        if let Err(e) = db.run(&query_str) {
            log::warn!("db-run failed: {}", e);
        }
    })?;
    Ok(vec![Value::null()])
}

// --- (canvas graph) bridge functions ---

#[bridge(name = "make-node", lib = "(canvas graph)")]
fn bridge_make_node(template: &Value, x: &Value, y: &Value) -> Result<Vec<Value>, Exception> {
    let template_name = value_to_string(template);
    let x_val = x.cast_to_scheme_type::<f64>().unwrap_or(100.0) as f32;
    let y_val = y.cast_to_scheme_type::<f64>().unwrap_or(100.0) as f32;
    with_graph(|graph, registry| {
        match registry.templates.get(&template_name) {
            Some(tmpl) => {
                let id = graph.add_node(tmpl, [x_val, y_val]);
                Value::from(id as f64)
            }
            None => {
                log::warn!("make-node: template '{}' not found", template_name);
                Value::from(-1.0f64)
            }
        }
    })
    .map(|v| vec![v])
}

#[bridge(name = "connect", lib = "(canvas graph)")]
fn bridge_connect(
    from: &Value, from_port: &Value,
    to: &Value, to_port: &Value,
) -> Result<Vec<Value>, Exception> {
    let from_id = from.cast_to_scheme_type::<f64>().unwrap_or(0.0) as NodeId;
    let from_port_name = value_to_string(from_port);
    let to_id = to.cast_to_scheme_type::<f64>().unwrap_or(0.0) as NodeId;
    let to_port_name = value_to_string(to_port);
    with_graph(|graph, _| {
        graph.add_connection(from_id, from_port_name, to_id, to_port_name);
    })?;
    Ok(vec![Value::null()])
}

#[bridge(name = "remove-node", lib = "(canvas graph)")]
fn bridge_remove_node(id: &Value) -> Result<Vec<Value>, Exception> {
    let node_id = id.cast_to_scheme_type::<f64>().unwrap_or(0.0) as NodeId;
    with_graph(|graph, _| {
        graph.remove_node(node_id);
    })?;
    Ok(vec![Value::null()])
}

#[bridge(name = "list-nodes", lib = "(canvas graph)")]
fn bridge_list_nodes() -> Result<Vec<Value>, Exception> {
    with_graph(|graph, _| {
        let mut entries: Vec<_> = graph.nodes.iter().collect();
        entries.sort_by_key(|(&id, _)| id);
        let mut list = Value::null();
        for (&id, node) in entries.into_iter().rev() {
            let id_val = Value::from(id as f64);
            let name_val = Value::from(node.label.clone());
            let tmpl_val = Value::from(node.template_name.clone());
            let entry = Value::from(scheme_rs::lists::Pair::new(
                id_val,
                Value::from(scheme_rs::lists::Pair::new(
                    name_val,
                    Value::from(scheme_rs::lists::Pair::new(tmpl_val, Value::null(), true)),
                    true,
                )),
                true,
            ));
            list = Value::from(scheme_rs::lists::Pair::new(entry, list, true));
        }
        list
    })
    .map(|v| vec![v])
}

pub(crate) fn types_value_to_scheme(v: &crate::types::Value) -> Value {
    match v {
        crate::types::Value::F64(f) => Value::from(*f),
        crate::types::Value::F32(f) => Value::from(*f as f64),
        crate::types::Value::I64(i) => Value::from(*i as f64),
        crate::types::Value::I32(i) => Value::from(*i as f64),
        crate::types::Value::Bool(b) => Value::from(*b),
        crate::types::Value::Str(s) => Value::from(s.clone()),
    }
}

#[bridge(name = "node-ref", lib = "(canvas graph)")]
fn bridge_node_ref(id: &Value, key: &Value) -> Result<Vec<Value>, Exception> {
    let node_id = id.cast_to_scheme_type::<f64>().unwrap_or(0.0) as NodeId;
    let key_str = value_to_string(key);
    with_graph(|graph, _| {
        match graph.nodes.get(&node_id) {
            Some(node) => match key_str.as_str() {
                "label" => Value::from(node.label.clone()),
                "template" => Value::from(node.template_name.clone()),
                "x" => Value::from(node.pos[0] as f64),
                "y" => Value::from(node.pos[1] as f64),
                "code" => Value::from(node.script_code.clone()),
                _ => {
                    // Check output_values
                    node.output_values
                        .get(&key_str)
                        .map(|v| types_value_to_scheme(v))
                        .unwrap_or(Value::null())
                }
            },
            None => Value::null(),
        }
    })
    .map(|v| vec![v])
}

#[bridge(name = "node-set!", lib = "(canvas graph)")]
fn bridge_node_set(id: &Value, key: &Value, value: &Value) -> Result<Vec<Value>, Exception> {
    let node_id = id.cast_to_scheme_type::<f64>().unwrap_or(0.0) as NodeId;
    let key_str = value_to_string(key);
    with_graph(|graph, _| {
        if let Some(node) = graph.nodes.get_mut(&node_id) {
            match key_str.as_str() {
                "label" => node.label = value_to_string(value),
                "x" => node.pos[0] = value.cast_to_scheme_type::<f64>().unwrap_or(0.0) as f32,
                "y" => node.pos[1] = value.cast_to_scheme_type::<f64>().unwrap_or(0.0) as f32,
                "code" => node.script_code = value_to_string(value),
                _ => {}
            }
        }
    })?;
    Ok(vec![Value::null()])
}

// --- (canvas ports) bridge functions ---

#[bridge(name = "register-input", lib = "(canvas ports)")]
fn bridge_register_input(name: &Value, port_type: &Value) -> Result<Vec<Value>, Exception> {
    let name_str = value_to_string(name);
    let type_str = value_to_string(port_type);

    THREAD_PORTS.with(|cell| {
        cell.borrow_mut().inputs.push((name_str.clone(), type_str));
    });

    let result = THREAD_INPUTS.with(|cell| {
        let borrow = cell.borrow();
        match borrow.as_ref() {
            Some(map) => match map.get(&name_str) {
                Some(val) => types_value_to_scheme(val),
                None => Value::from(String::from("<compute>")),
            },
            None => Value::from(String::from("<compute>")),
        }
    });

    Ok(vec![result])
}

#[bridge(name = "register-output", lib = "(canvas ports)")]
fn bridge_register_output(name: &Value, port_type: &Value) -> Result<Vec<Value>, Exception> {
    let name_str = value_to_string(name);
    let type_str = value_to_string(port_type);

    THREAD_PORTS.with(|cell| {
        cell.borrow_mut().outputs.push((name_str, type_str));
    });

    Ok(vec![Value::null()])
}

#[bridge(name = "register-widget", lib = "(canvas ports)")]
fn bridge_register_widget(
    name: &Value, wtype: &Value, p1: &Value, p2: &Value,
) -> Result<Vec<Value>, Exception> {
    let name_str = value_to_string(name);
    let type_str = value_to_string(wtype);
    let p1_f64 = p1.cast_to_scheme_type::<f64>().unwrap_or(0.0);
    let p2_f64 = p2.cast_to_scheme_type::<f64>().unwrap_or(0.0);

    THREAD_PORTS.with(|cell| {
        let mut ports = cell.borrow_mut();
        ports.widgets.push(WidgetDecl {
            name: name_str.clone(),
            widget_type: type_str,
            params: vec![p1_f64, p2_f64],
        });
        ports.outputs.push((name_str.clone(), "f64".to_string()));
    });

    let result = THREAD_INPUTS.with(|cell| {
        let borrow = cell.borrow();
        match borrow.as_ref() {
            Some(map) => match map.get(&name_str) {
                Some(val) => types_value_to_scheme(val),
                None => Value::from(p1_f64),
            },
            None => Value::from(p1_f64),
        }
    });

    Ok(vec![result])
}

// --- (canvas net) bridge functions ---

#[bridge(name = "net-publish-channel", lib = "(canvas net)")]
fn bridge_net_publish(channel: &Value) -> Result<Vec<Value>, Exception> {
    let ch = value_to_string(channel);
    THREAD_NET_PUBLISH.with(|cell| {
        cell.borrow_mut().push(ch);
    });
    Ok(vec![Value::null()])
}

#[bridge(name = "net-value-get", lib = "(canvas net)")]
fn bridge_net_value(channel: &Value, key: &Value, default: &Value) -> Result<Vec<Value>, Exception> {
    let ch = value_to_string(channel);
    let k = value_to_string(key);

    let result = THREAD_NET_VALUES.with(|cell| {
        let borrow = cell.borrow();
        match borrow.as_ref() {
            Some(nv) => {
                let store = nv.lock().unwrap();
                // Search all peers for this channel
                for ((_, channel), values) in store.iter() {
                    if *channel == ch {
                        if let Some(val) = values.get(&k) {
                            return types_value_to_scheme(val);
                        }
                    }
                }
                default.clone()
            }
            None => default.clone(),
        }
    });

    Ok(vec![result])
}
