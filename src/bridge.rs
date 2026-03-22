use crate::db::Db;
use crate::registry::NodeRegistry;
use crate::types::{Graph, NodeId};
use scheme_rs::exceptions::Exception;
use scheme_rs::registry::bridge;
use scheme_rs::value::Value;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Shared network values store: (peer, channel) -> (key -> Value)
pub type NetValues = Arc<Mutex<HashMap<(String, String), HashMap<String, crate::types::Value>>>>;

/// Shared OCapN slot store: swiss-hex -> mutable slot value
pub type OCapNSlotStore = Arc<Mutex<HashMap<String, Arc<Mutex<crate::ocapn::syrup::SyrupValue>>>>>;

/// Mapping: swiss-hex -> owner node ID (for routing OCapN delivers to actor mailboxes)
pub type OCapNSlotOwners = Arc<Mutex<HashMap<String, crate::types::NodeId>>>;

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

/// Queued OCapN send: (peer_id, OCapNMessage)
pub type OCapNSendEntry = (String, crate::ocapn::types::OCapNMessage);

/// Shared connected peers set
pub type ConnectedPeers = Arc<Mutex<HashSet<String>>>;

/// Shared OCapN call results store
pub type OCapNCallResults = Arc<Mutex<HashMap<u64, crate::ocapn::syrup::SyrupValue>>>;

/// Per-node actor mailbox: node_id -> queue of messages (each message = Vec<SyrupValue>)
pub type NodeMailboxes = Arc<Mutex<HashMap<crate::types::NodeId, std::collections::VecDeque<Vec<crate::ocapn::syrup::SyrupValue>>>>>;

// Port-context thread-locals (scope-guarded, stay separate from ActorContext)
thread_local! {
    static THREAD_INPUTS: RefCell<Option<HashMap<String, crate::types::Value>>> = RefCell::new(None);
    static THREAD_PORTS: RefCell<PortRegistry> = RefCell::new(PortRegistry::new());
}

// --- ActorContext-backed accessors ---
// All bridge functions use with_actor_ctx() from actor.rs.
// These public functions are kept for callers (app.rs, worker.rs, scheme_engine.rs).

/// Take collected net-publish channel names from actor context.
pub fn take_net_publishes() -> Vec<String> {
    crate::actor::with_actor_ctx(|ctx| std::mem::take(&mut ctx.net_publishes))
        .unwrap_or_default()
}

/// Take requested tick interval from actor context.
pub fn take_tick_interval() -> Option<u64> {
    crate::actor::with_actor_ctx(|ctx| ctx.tick_interval_ms.take())
        .flatten()
}

/// Take collected OCapN send commands from actor context.
pub fn take_ocapn_sends() -> Vec<OCapNSendEntry> {
    crate::actor::with_actor_ctx(|ctx| std::mem::take(&mut ctx.ocapn_sends))
        .unwrap_or_default()
}

/// Take collected recompute requests from actor context.
pub fn take_recompute_requests() -> Vec<crate::types::NodeId> {
    crate::actor::with_actor_ctx(|ctx| std::mem::take(&mut ctx.recompute_requests))
        .unwrap_or_default()
}

/// Take window title from actor context.
pub fn take_window_title() -> Option<String> {
    crate::actor::with_actor_ctx(|ctx| ctx.window_title.take())
        .flatten()
}

/// Take the has_message_handler flag from actor context.
pub fn take_has_message_handler() -> bool {
    crate::actor::with_actor_ctx(|ctx| {
        let val = ctx.has_message_handler;
        ctx.has_message_handler = false;
        val
    })
    .unwrap_or(false)
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
    crate::actor::with_actor_ctx(|ctx| {
        ctx.net_publishes.push(ch);
    });
    Ok(vec![Value::null()])
}

#[bridge(name = "net-value-get", lib = "(canvas net)")]
fn bridge_net_value(channel: &Value, key: &Value, default: &Value) -> Result<Vec<Value>, Exception> {
    let ch = value_to_string(channel);
    let k = value_to_string(key);

    let result = crate::actor::with_actor_ctx(|ctx| {
        match ctx.net_values.as_ref() {
            Some(nv) => {
                let store = nv.lock().unwrap();
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
    })
    .unwrap_or_else(|| default.clone());

    Ok(vec![result])
}

// --- (canvas ocapn) bridge functions ---

use crate::ocapn::session::SessionManager;
use crate::ocapn::syrup::SyrupValue;

pub type SharedSessionManager = Arc<Mutex<SessionManager>>;

fn with_session_mgr<R>(f: impl FnOnce(&mut SessionManager) -> R) -> Result<R, Exception> {
    crate::actor::with_actor_ctx(|ctx| {
        ctx.session_mgr.as_ref().map(|mgr| f(&mut mgr.lock().unwrap()))
    })
    .flatten()
    .ok_or_else(|| Exception::error("No OCapN session manager available"))
}

/// Mutable slot exported via OCapN. Supports "set" and "get" methods.
struct ExportedSlot {
    value: Arc<Mutex<SyrupValue>>,
}

impl crate::ocapn::session::OCapNObject for ExportedSlot {
    fn deliver(&self, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
        // Check if first arg is method name
        let method = args.first().and_then(|a| {
            if let SyrupValue::Symbol(s) = a { Some(s.as_str()) } else { None }
        });

        match method {
            Some("set") if args.len() >= 2 => {
                let mut val = self.value.lock().unwrap();
                *val = args[1].clone();
                Ok(None)
            }
            _ => {
                // "get" or no args → return current value
                let val = self.value.lock().unwrap();
                Ok(Some(val.clone()))
            }
        }
    }
}

/// Convert SyrupValue to Scheme Value
fn syrup_value_to_scheme(sv: &SyrupValue) -> Value {
    match sv {
        SyrupValue::Float64(f) => Value::from(*f),
        SyrupValue::Float32(f) => Value::from(*f as f64),
        SyrupValue::Integer(i) => Value::from(*i as f64),
        SyrupValue::String(s) => Value::from(s.clone()),
        SyrupValue::Bool(b) => Value::from(*b),
        SyrupValue::Symbol(s) => Value::from(s.clone()),
        _ => Value::from(format!("{:?}", sv)),
    }
}

/// Convert a Scheme list (pair chain) to Vec<SyrupValue>
fn scheme_list_to_syrup_vec(val: &Value) -> Vec<SyrupValue> {
    use scheme_rs::value::UnpackedValue;
    let mut result = Vec::new();
    let mut current = val.clone();
    loop {
        match current.clone().unpack() {
            UnpackedValue::Pair(p) => {
                let item = &p.car();
                result.push(SyrupValue::from(&crate::scheme_engine::scheme_value_to_types_value(item)));
                current = p.cdr();
            }
            UnpackedValue::Null => break,
            _ => {
                result.push(SyrupValue::from(&crate::scheme_engine::scheme_value_to_types_value(&current)));
                break;
            }
        }
    }
    result
}

#[bridge(name = "ocapn-export-value", lib = "(canvas ocapn)")]
fn bridge_ocapn_export(value: &Value) -> Result<Vec<Value>, Exception> {
    let syrup_val = SyrupValue::from(&crate::scheme_engine::scheme_value_to_types_value(value));

    // Generate stable key from node_id + export counter
    let export_key = crate::actor::with_actor_ctx(|ctx| {
        let counter = ctx.next_export_counter();
        format!("node:{}:export:{}", ctx.node_id, counter)
    })
    .unwrap_or_else(|| "node:0:export:0".into());

    let result = with_session_mgr(|mgr| {
        let peer_id = mgr.local_peer_id().unwrap_or("local").to_string();
        let session = mgr.local_session_mut();

        // Check if we already have a keyed export — reuse the slot Arc
        let swiss_hex = if let Some(existing_swiss) = session.export_by_key.get(&export_key) {
            let swiss_hex = existing_swiss.to_hex();
            // Update the slot value in-place via ActorContext
            crate::actor::with_actor_ctx(|ctx| {
                if let Some(store) = ctx.ocapn_slots.as_ref() {
                    let store = store.lock().unwrap();
                    if let Some(slot) = store.get(&swiss_hex) {
                        *slot.lock().unwrap() = syrup_val;
                    }
                }
            });
            swiss_hex
        } else {
            let slot_value = Arc::new(Mutex::new(syrup_val));
            let slot = ExportedSlot { value: Arc::clone(&slot_value) };
            let (_pos, swiss) = session.export_object_keyed(export_key, Box::new(slot));
            let swiss_hex = swiss.to_hex();

            crate::actor::with_actor_ctx(|ctx| {
                if let Some(store) = ctx.ocapn_slots.as_ref() {
                    store.lock().unwrap().insert(swiss_hex.clone(), Arc::clone(&slot_value));
                }
            });
            swiss_hex
        };

        // Record owner node_id for OCapN → mailbox routing
        crate::actor::with_actor_ctx(|ctx| {
            if let Some(owners) = ctx.slot_owners.as_ref() {
                owners.lock().unwrap().insert(swiss_hex.clone(), ctx.node_id);
            }
        });

        format!("ocapn://{}.libp2p/s/{}", peer_id, swiss_hex)
    })?;

    Ok(vec![Value::from(result)])
}

#[bridge(name = "ocapn-receive-msg", lib = "(canvas ocapn)")]
fn bridge_ocapn_receive(uri: &Value, default: &Value) -> Result<Vec<Value>, Exception> {
    let uri_str = value_to_string(uri);

    // Parse swiss-hex from URI: "ocapn://...libp2p/s/<hex>"
    let swiss_hex = uri_str.rsplit("/s/").next()
        .ok_or_else(|| Exception::error("ocapn-receive: invalid URI format"))?
        .to_string();

    let result = crate::actor::with_actor_ctx(|ctx| {
        match ctx.ocapn_slots.as_ref() {
            Some(store) => {
                let store = store.lock().unwrap();
                match store.get(&swiss_hex) {
                    Some(slot) => {
                        let val = slot.lock().unwrap();
                        syrup_value_to_scheme(&val)
                    }
                    None => default.clone(),
                }
            }
            None => default.clone(),
        }
    })
    .unwrap_or_else(|| default.clone());

    Ok(vec![result])
}

#[bridge(name = "ocapn-send-msg", lib = "(canvas ocapn)")]
fn bridge_ocapn_send(locator_str: &Value, method: &Value, args_list: &Value) -> Result<Vec<Value>, Exception> {
    use crate::ocapn::locator::OCapNLocator;
    use crate::ocapn::types::{Descriptor, OCapNMessage};

    let loc_str = value_to_string(locator_str);
    let method_str = value_to_string(method);

    let locator = OCapNLocator::parse(&loc_str)
        .ok_or_else(|| Exception::error(&format!("ocapn-send: invalid URI: {}", loc_str)))?;

    let swiss = locator.swiss_num
        .ok_or_else(|| Exception::error("ocapn-send: URI has no swiss-num"))?;

    // Convert Scheme args list to SyrupValue vec
    let user_args = scheme_list_to_syrup_vec(args_list);

    // Build OpDeliverOnly: [Symbol("deliver-to"), swiss, Symbol(method), ...user_args]
    let mut args = vec![
        SyrupValue::Symbol("deliver-to".into()),
        swiss.to_syrup(),
        SyrupValue::Symbol(method_str),
    ];
    args.extend(user_args);

    let msg = OCapNMessage::OpDeliverOnly {
        to_desc: Descriptor::Export(0),
        args,
    };

    let peer_id = locator.designator;

    crate::actor::with_actor_ctx(|ctx| {
        ctx.ocapn_sends.push((peer_id.clone(), msg));
    });

    log::info!("ocapn-send: queued message to {}", peer_id);
    Ok(vec![Value::null()])
}

#[bridge(name = "ocapn-export-node-id", lib = "(canvas ocapn)")]
fn bridge_ocapn_export_node(node_id_val: &Value) -> Result<Vec<Value>, Exception> {
    let node_id = node_id_val.cast_to_scheme_type::<f64>().unwrap_or(0.0) as crate::types::NodeId;

    // Read node output values from graph context
    let output_values = with_graph(|graph, _| {
        graph.nodes.get(&node_id).map(|n| n.output_values.clone())
    })?;

    let output_values = output_values
        .ok_or_else(|| Exception::error(&format!("ocapn-export-node: node {} not found", node_id)))?;

    // Build SyrupValue::Dict from output_values
    let dict_entries: Vec<(SyrupValue, SyrupValue)> = output_values.iter()
        .map(|(k, v)| (SyrupValue::String(k.clone()), SyrupValue::from(v)))
        .collect();
    let syrup_val = SyrupValue::Dict(dict_entries);
    let slot_value = Arc::new(Mutex::new(syrup_val));

    let result = with_session_mgr(|mgr| {
        let peer_id = mgr.local_peer_id().unwrap_or("local").to_string();
        let session = mgr.local_session_mut();
        let slot = ExportedSlot { value: Arc::clone(&slot_value) };
        let (_pos, swiss) = session.export_object(Box::new(slot));
        let swiss_hex = swiss.to_hex();

        crate::actor::with_actor_ctx(|ctx| {
            if let Some(store) = ctx.ocapn_slots.as_ref() {
                store.lock().unwrap().insert(swiss_hex.clone(), Arc::clone(&slot_value));
            }
        });

        format!("ocapn://{}.libp2p/s/{}", peer_id, swiss_hex)
    })?;

    Ok(vec![Value::from(result)])
}

#[bridge(name = "ocapn-peers-list", lib = "(canvas ocapn)")]
fn bridge_ocapn_peers() -> Result<Vec<Value>, Exception> {
    let result = crate::actor::with_actor_ctx(|ctx| {
        match ctx.connected_peers.as_ref() {
            Some(peers) => {
                let peers = peers.lock().unwrap();
                let mut list = Value::null();
                for peer in peers.iter() {
                    list = Value::from(scheme_rs::lists::Pair::new(
                        Value::from(peer.clone()),
                        list,
                        true,
                    ));
                }
                list
            }
            None => Value::null(),
        }
    })
    .unwrap_or_else(Value::null);
    Ok(vec![result])
}

#[bridge(name = "ocapn-local-id-get", lib = "(canvas ocapn)")]
fn bridge_ocapn_local_id() -> Result<Vec<Value>, Exception> {
    let result = with_session_mgr(|mgr| {
        Value::from(mgr.local_peer_id().unwrap_or("").to_string())
    })?;
    Ok(vec![result])
}

#[bridge(name = "ocapn-call-msg", lib = "(canvas ocapn)")]
fn bridge_ocapn_call(locator_str: &Value, method: &Value, args_list: &Value) -> Result<Vec<Value>, Exception> {
    use crate::ocapn::locator::OCapNLocator;
    use crate::ocapn::types::{Descriptor, OCapNMessage};

    let loc_str = value_to_string(locator_str);
    let method_str = value_to_string(method);

    let locator = OCapNLocator::parse(&loc_str)
        .ok_or_else(|| Exception::error(&format!("ocapn-call: invalid URI: {}", loc_str)))?;

    let swiss = locator.swiss_num
        .ok_or_else(|| Exception::error("ocapn-call: URI has no swiss-num"))?;

    let user_args = scheme_list_to_syrup_vec(args_list);

    // Generate a request_id
    let request_id = rand::random::<u64>();

    let mut args = vec![
        SyrupValue::Symbol("deliver-to".into()),
        swiss.to_syrup(),
        SyrupValue::Symbol(method_str),
    ];
    args.extend(user_args);

    let msg = OCapNMessage::OpDeliver {
        to_desc: Descriptor::Export(0),
        args,
        request_id,
    };

    let peer_id = locator.designator;

    crate::actor::with_actor_ctx(|ctx| {
        ctx.ocapn_sends.push((peer_id.clone(), msg));
    });

    log::info!("ocapn-call: queued request {} to {}", request_id, peer_id);
    Ok(vec![Value::from(request_id as f64)])
}

#[bridge(name = "ocapn-call-result-get", lib = "(canvas ocapn)")]
fn bridge_ocapn_call_result(request_id: &Value, default: &Value) -> Result<Vec<Value>, Exception> {
    let rid = request_id.cast_to_scheme_type::<f64>().unwrap_or(0.0) as u64;

    let result = crate::actor::with_actor_ctx(|ctx| {
        match ctx.ocapn_call_results.as_ref() {
            Some(store) => {
                let store = store.lock().unwrap();
                match store.get(&rid) {
                    Some(val) => syrup_value_to_scheme(val),
                    None => default.clone(),
                }
            }
            None => default.clone(),
        }
    })
    .unwrap_or_else(|| default.clone());

    Ok(vec![result])
}

// --- (canvas actor) bridge functions ---

#[bridge(name = "actor-node-id", lib = "(canvas actor)")]
fn bridge_actor_node_id() -> Result<Vec<Value>, Exception> {
    let id = crate::actor::with_actor_ctx(|ctx| ctx.node_id).unwrap_or(0);
    Ok(vec![Value::from(id as f64)])
}

#[bridge(name = "actor-node-address", lib = "(canvas actor)")]
fn bridge_actor_node_address() -> Result<Vec<Value>, Exception> {
    let node_id = crate::actor::with_actor_ctx(|ctx| ctx.node_id).unwrap_or(0);
    let result = with_session_mgr(|mgr| {
        let peer_id = mgr.local_peer_id().unwrap_or("local").to_string();
        format!("actor://{}/{}", peer_id, node_id)
    })?;
    Ok(vec![Value::from(result)])
}

#[bridge(name = "actor-send-msg", lib = "(canvas actor)")]
fn bridge_actor_send(target_id: &Value, method: &Value, args_list: &Value) -> Result<Vec<Value>, Exception> {
    let tid = target_id.cast_to_scheme_type::<f64>().unwrap_or(0.0) as crate::types::NodeId;
    let method_str = value_to_string(method);

    let mut msg_args = vec![SyrupValue::Symbol(method_str)];
    msg_args.extend(scheme_list_to_syrup_vec(args_list));

    crate::actor::with_actor_ctx(|ctx| {
        if let Some(mailboxes) = ctx.node_mailboxes.as_ref() {
            mailboxes.lock().unwrap().entry(tid).or_default().push_back(msg_args);
        }
        ctx.recompute_requests.push(tid);
    });

    Ok(vec![Value::null()])
}

#[bridge(name = "actor-receive-msg", lib = "(canvas actor)")]
fn bridge_actor_receive() -> Result<Vec<Value>, Exception> {
    let result = crate::actor::with_actor_ctx(|ctx| {
        let node_id = ctx.node_id;
        match ctx.node_mailboxes.as_ref() {
            Some(mailboxes) => {
                let mut mailboxes = mailboxes.lock().unwrap();
                if let Some(queue) = mailboxes.get_mut(&node_id) {
                    if let Some(msg) = queue.pop_front() {
                        let mut list = Value::null();
                        for item in msg.into_iter().rev() {
                            list = Value::from(scheme_rs::lists::Pair::new(
                                syrup_value_to_scheme(&item),
                                list,
                                true,
                            ));
                        }
                        return list;
                    }
                }
                Value::from(false)
            }
            None => Value::from(false),
        }
    })
    .unwrap_or_else(|| Value::from(false));

    Ok(vec![result])
}

#[bridge(name = "actor-mailbox-count", lib = "(canvas actor)")]
fn bridge_actor_mailbox_count() -> Result<Vec<Value>, Exception> {
    let count = crate::actor::with_actor_ctx(|ctx| {
        let node_id = ctx.node_id;
        match ctx.node_mailboxes.as_ref() {
            Some(mailboxes) => mailboxes.lock().unwrap().get(&node_id).map_or(0, |q| q.len()),
            None => 0,
        }
    })
    .unwrap_or(0);

    Ok(vec![Value::from(count as f64)])
}

#[bridge(name = "actor-register-handler", lib = "(canvas actor)")]
fn bridge_actor_register_handler() -> Result<Vec<Value>, Exception> {
    crate::actor::with_actor_ctx(|ctx| {
        ctx.has_message_handler = true;
    });
    Ok(vec![Value::null()])
}

#[bridge(name = "actor-self-send-msg", lib = "(canvas actor)")]
fn bridge_actor_self_send(method: &Value, args_list: &Value) -> Result<Vec<Value>, Exception> {
    let method_str = value_to_string(method);

    let mut msg_args = vec![SyrupValue::Symbol(method_str)];
    msg_args.extend(scheme_list_to_syrup_vec(args_list));

    crate::actor::with_actor_ctx(|ctx| {
        let node_id = ctx.node_id;
        if let Some(mailboxes) = ctx.node_mailboxes.as_ref() {
            mailboxes.lock().unwrap().entry(node_id).or_default().push_back(msg_args);
        }
        ctx.recompute_requests.push(node_id);
    });

    Ok(vec![Value::null()])
}

#[bridge(name = "actor-open-window", lib = "(canvas actor)")]
fn bridge_open_window(title: &Value) -> Result<Vec<Value>, Exception> {
    let title = value_to_string(title);
    crate::actor::with_actor_ctx(|ctx| {
        ctx.window_title = Some(title);
    });
    Ok(vec![Value::null()])
}

// --- (canvas timer) bridge function ---

#[bridge(name = "request-tick-ms", lib = "(canvas timer)")]
fn bridge_request_tick(ms: &Value) -> Result<Vec<Value>, Exception> {
    let interval = ms.cast_to_scheme_type::<f64>().unwrap_or(100.0) as u64;
    crate::actor::with_actor_ctx(|ctx| {
        ctx.tick_interval_ms = Some(interval);
    });
    Ok(vec![Value::null()])
}
