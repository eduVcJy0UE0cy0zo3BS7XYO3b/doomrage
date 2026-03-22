use crate::bridge::{
    ConnectedPeers, NodeMailboxes, OCapNCallResults, OCapNSendEntry, OCapNSlotOwners,
    OCapNSlotStore, SharedSessionManager,
};
use crate::bridge::NetValues;
use crate::db::Db;
use crate::executor::WasmRunner;
use crate::ocapn::syrup::SyrupValue;
use crate::render::RenderBlock;
use crate::scheme_engine::{SchemeEngine, ScriptResult};
use crate::types::{BuiltinKind, NodeId, NodeTemplate, Value};
use scheme_rs::env::TopLevelEnvironment;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{mpsc, Arc, Mutex};

/// Per-node actor context. Replaces 10+ individual thread-locals with a single struct.
/// Holds both shared resources (Arc<Mutex<>>) and per-eval mutable outputs.
pub struct ActorContext {
    // --- Identity ---
    pub node_id: NodeId,
    pub export_counter: u32,

    // --- Shared resources (read) ---
    pub net_values: Option<NetValues>,
    pub session_mgr: Option<SharedSessionManager>,
    pub ocapn_slots: Option<OCapNSlotStore>,
    pub connected_peers: Option<ConnectedPeers>,
    pub ocapn_call_results: Option<OCapNCallResults>,
    pub node_mailboxes: Option<NodeMailboxes>,
    pub slot_owners: Option<OCapNSlotOwners>,

    // --- Per-eval outputs (collected during eval, taken after) ---
    pub ocapn_sends: Vec<OCapNSendEntry>,
    pub net_publishes: Vec<String>,
    pub tick_interval_ms: Option<u64>,
    pub recompute_requests: Vec<NodeId>,
    pub has_message_handler: bool,
    pub window_title: Option<String>,
}

impl ActorContext {
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            export_counter: 0,
            net_values: None,
            session_mgr: None,
            ocapn_slots: None,
            connected_peers: None,
            ocapn_call_results: None,
            node_mailboxes: None,
            slot_owners: None,
            ocapn_sends: Vec::new(),
            net_publishes: Vec::new(),
            tick_interval_ms: None,
            recompute_requests: Vec::new(),
            has_message_handler: false,
            window_title: None,
        }
    }

    /// Reset per-eval output fields. Call before each eval.
    pub fn reset_outputs(&mut self) {
        self.export_counter = 0;
        self.ocapn_sends.clear();
        self.net_publishes.clear();
        self.tick_interval_ms = None;
        self.recompute_requests.clear();
        self.has_message_handler = false;
        self.window_title = None;
    }

    /// Take all collected outputs, leaving empty defaults.
    pub fn take_outputs(&mut self) -> ActorOutputs {
        ActorOutputs {
            ocapn_sends: std::mem::take(&mut self.ocapn_sends),
            net_publishes: std::mem::take(&mut self.net_publishes),
            tick_interval_ms: self.tick_interval_ms.take(),
            recompute_requests: std::mem::take(&mut self.recompute_requests),
            has_message_handler: self.has_message_handler,
            window_title: self.window_title.take(),
        }
    }

    // --- Accessors used by bridge functions ---

    pub fn next_export_counter(&mut self) -> u32 {
        let val = self.export_counter;
        self.export_counter += 1;
        val
    }
}

/// Collected outputs from a single eval.
pub struct ActorOutputs {
    pub ocapn_sends: Vec<OCapNSendEntry>,
    pub net_publishes: Vec<String>,
    pub tick_interval_ms: Option<u64>,
    pub recompute_requests: Vec<NodeId>,
    pub has_message_handler: bool,
    pub window_title: Option<String>,
}

// --- Thread-local singleton ---

thread_local! {
    static THREAD_ACTOR_CTX: RefCell<Option<*mut ActorContext>> = RefCell::new(None);
}

/// Set the thread-local actor context pointer. SAFETY: caller must ensure
/// the pointer remains valid for the duration (use `with_actor_context` scope-guard).
pub unsafe fn set_thread_actor_ctx(ctx: Option<*mut ActorContext>) {
    THREAD_ACTOR_CTX.with(|cell| {
        *cell.borrow_mut() = ctx;
    });
}

/// Access the current actor context. Returns Err if no context is set.
pub fn with_actor_ctx<R>(f: impl FnOnce(&mut ActorContext) -> R) -> Option<R> {
    THREAD_ACTOR_CTX.with(|cell| {
        let borrow = cell.borrow();
        match *borrow {
            Some(ptr) => {
                // SAFETY: pointer is valid within with_actor_context scope
                let ctx = unsafe { &mut *ptr };
                Some(f(ctx))
            }
            None => None,
        }
    })
}

/// Scope-guard: sets actor context before `f`, clears after.
pub fn with_actor_context<R>(ctx: &mut ActorContext, f: impl FnOnce() -> R) -> R {
    unsafe { set_thread_actor_ctx(Some(ctx as *mut ActorContext)); }
    let result = f();
    unsafe { set_thread_actor_ctx(None); }
    result
}

// SAFETY: ActorContext pointers are only used within with_actor_context scope
// and never escape the thread they were created on.

// --- ActorRuntime: per-node async dispatch ---

/// Internal message — not exposed to callers.
struct ActorMsg {
    node: crate::types::Node,
    template: Option<NodeTemplate>,
    available_inputs: HashMap<String, Value>,
    db: Db,
    immediate: bool,
    fast_message_path: bool,
    /// Labels of connected source nodes (for auto-import)
    connected_modules: Vec<String>,
}

/// Result from processing an actor message.
pub enum ActorResult {
    Computed {
        node_id: NodeId,
        result: ScriptResult,
        /// Env after eval — cache for next compute.
        env: TopLevelEnvironment,
        /// Hash of code that produced this env.
        code_hash: u64,
        /// Cached preprocessed code for fast re-eval.
        preprocessed: Option<String>,
    },
    Error {
        node_id: NodeId,
        message: String,
    },
}

fn hash_code(code: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    hasher.finish()
}

/// Shared resources cloned into each actor's context.
#[derive(Clone)]
struct SharedResources {
    net_values: Option<NetValues>,
    session_mgr: Option<SharedSessionManager>,
    ocapn_slots: Option<OCapNSlotStore>,
    connected_peers: Option<ConnectedPeers>,
    ocapn_call_results: Option<OCapNCallResults>,
    node_mailboxes: Option<NodeMailboxes>,
    slot_owners: Option<OCapNSlotOwners>,
    wasm: Option<WasmRunner>,
    egui_ctx: Option<egui::Context>,
}

/// Cached environment for a node.
struct CachedEnv {
    code_hash: u64,
    env: TopLevelEnvironment,
    /// Cached preprocessed code (skip Scribble on re-eval)
    preprocessed: Option<String>,
}

/// Default debounce delay for user-triggered recomputes (slider, widget).
const DEFAULT_DEBOUNCE_MS: u64 = 50;

/// Pending compute waiting for debounce timer to expire.
struct PendingCompute {
    msg: ActorMsg,
    scheduled_at: std::time::Instant,
}

/// Per-node tracking: is an eval currently in-flight?
struct NodeState {
    in_flight: bool,
    /// If new data arrived while eval was in-flight, store it here.
    dirty: Option<PendingCompute>,
}

/// Runtime that manages per-node actor execution on worker threads.
pub struct ActorRuntime {
    /// Scheme engine shared across all actor threads
    engine: Arc<SchemeEngine>,
    /// Thread pool for eval execution
    pool: rayon::ThreadPool,
    /// Channel for receiving completed results from worker threads
    result_rx: mpsc::Receiver<ActorResult>,
    result_tx: mpsc::Sender<ActorResult>,
    /// Shared resources
    shared: SharedResources,
    /// Per-node cached environments (persistent Scheme state)
    env_cache: HashMap<NodeId, CachedEnv>,
    /// Per-node in-flight tracking + coalesce
    node_states: HashMap<NodeId, NodeState>,
    /// Debounced computes waiting to fire
    debounce_queue: HashMap<NodeId, PendingCompute>,
    /// Debounce delay in milliseconds (0 = immediate)
    debounce_ms: u64,
    /// Nodes that have on-message handlers (for fast path)
    handler_nodes: std::collections::HashSet<NodeId>,
}

impl ActorRuntime {
    pub fn new(engine: Arc<SchemeEngine>) -> Self {
        Self::with_debounce(engine, DEFAULT_DEBOUNCE_MS)
    }

    pub fn with_debounce(engine: Arc<SchemeEngine>, debounce_ms: u64) -> Self {
        let (result_tx, result_rx) = mpsc::channel();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("Failed to create thread pool");
        Self {
            engine,
            pool,
            result_rx,
            result_tx,
            shared: SharedResources {
                net_values: None,
                session_mgr: None,
                ocapn_slots: None,
                connected_peers: None,
                ocapn_call_results: None,
                node_mailboxes: None,
                slot_owners: None,
                wasm: None,
                egui_ctx: None,
            },
            env_cache: HashMap::new(),
            node_states: HashMap::new(),
            debounce_queue: HashMap::new(),
            debounce_ms,
            handler_nodes: std::collections::HashSet::new(),
        }
    }

    pub fn set_net_values(&mut self, nv: NetValues) { self.shared.net_values = Some(nv); }
    pub fn set_session_mgr(&mut self, mgr: SharedSessionManager) { self.shared.session_mgr = Some(mgr); }
    pub fn set_ocapn_slots(&mut self, s: OCapNSlotStore) { self.shared.ocapn_slots = Some(s); }
    pub fn set_connected_peers(&mut self, p: ConnectedPeers) { self.shared.connected_peers = Some(p); }
    pub fn set_ocapn_call_results(&mut self, r: OCapNCallResults) { self.shared.ocapn_call_results = Some(r); }
    pub fn set_node_mailboxes(&mut self, m: NodeMailboxes) { self.shared.node_mailboxes = Some(m); }
    pub fn set_wasm_runner(&mut self, w: WasmRunner) { self.shared.wasm = Some(w); }
    pub fn set_slot_owners(&mut self, o: OCapNSlotOwners) { self.shared.slot_owners = Some(o); }
    pub fn set_egui_ctx(&mut self, ctx: egui::Context) { self.shared.egui_ctx = Some(ctx); }

    /// Compute a node immediately (for cascade, node-send, programmatic triggers).
    pub fn compute(
        &mut self,
        node_id: NodeId,
        node: crate::types::Node,
        template: Option<NodeTemplate>,
        available_inputs: HashMap<String, Value>,
        db: Db,
        connected_modules: Vec<String>,
    ) {
        self.enqueue(node_id, node, template, available_inputs, db, true, connected_modules);
    }

    /// Compute a node with debounce (for slider/widget user interaction).
    pub fn compute_debounced(
        &mut self,
        node_id: NodeId,
        node: crate::types::Node,
        template: Option<NodeTemplate>,
        available_inputs: HashMap<String, Value>,
        db: Db,
        connected_modules: Vec<String>,
    ) {
        self.enqueue(node_id, node, template, available_inputs, db, false, connected_modules);
    }

    fn enqueue(
        &mut self,
        node_id: NodeId,
        node: crate::types::Node,
        template: Option<NodeTemplate>,
        available_inputs: HashMap<String, Value>,
        db: Db,
        immediate: bool,
        connected_modules: Vec<String>,
    ) {
        // Detect fast message path internally
        let fast_message_path = self.should_fast_path(node_id, &node);

        let msg = ActorMsg {
            node, template, available_inputs, db,
            immediate, fast_message_path, connected_modules,
        };

        let state = self.node_states.entry(node_id).or_insert(NodeState {
            in_flight: false,
            dirty: None,
        });

        let pending = PendingCompute {
            msg,
            scheduled_at: std::time::Instant::now(),
        };

        if state.in_flight {
            state.dirty = Some(pending);
        } else if immediate {
            self.debounce_queue.remove(&node_id);
            self.spawn_eval(node_id, pending);
        } else {
            self.debounce_queue.insert(node_id, pending);
        }
    }

    /// Check if a node qualifies for fast message-handler path.
    /// Fast path: drain messages + re-eval with cached preprocessed code.
    fn should_fast_path(&self, node_id: NodeId, node: &crate::types::Node) -> bool {
        if !self.handler_nodes.contains(&node_id) {
            return false;
        }
        let code_hash = hash_code(&node.script_code);
        let env_matches = self.env_cache.get(&node_id)
            .map_or(false, |c| c.code_hash == code_hash);
        let has_messages = self.shared.node_mailboxes.as_ref()
            .map_or(false, |mb| mb.lock().unwrap().get(&node_id).map_or(false, |q| !q.is_empty()));
        env_matches && has_messages
    }

    /// Flush debounced computes whose timer has expired. Call every frame.
    pub fn flush_debounced(&mut self) {
        let now = std::time::Instant::now();
        let ready: Vec<NodeId> = self.debounce_queue.iter()
            .filter(|(_, p)| now.duration_since(p.scheduled_at).as_millis() as u64 >= self.debounce_ms)
            .map(|(id, _)| *id)
            .collect();

        for node_id in ready {
            if let Some(pending) = self.debounce_queue.remove(&node_id) {
                self.spawn_eval(node_id, pending);
            }
        }
    }

    /// Cancel all pending and debounced work.
    pub fn cancel_all(&mut self) {
        self.debounce_queue.clear();
        for state in self.node_states.values_mut() {
            state.dirty = None;
        }
        while self.result_rx.try_recv().is_ok() {}
    }

    /// Remove an actor (cleanup on node delete).
    pub fn remove(&mut self, node_id: NodeId) {
        self.env_cache.remove(&node_id);
        self.node_states.remove(&node_id);
        self.debounce_queue.remove(&node_id);
    }

    /// Whether any work is in-flight or debounce-pending.
    pub fn has_pending(&self) -> bool {
        !self.debounce_queue.is_empty()
            || self.node_states.values().any(|s| s.in_flight || s.dirty.is_some())
    }

    /// Non-blocking poll for completed results.
    /// Handles coalesce: if node was marked dirty while in-flight, re-spawns.
    pub fn poll(&mut self) -> Option<ActorResult> {
        // First flush any ready debounced computes
        self.flush_debounced();

        match self.result_rx.try_recv() {
            Ok(result) => {
                let node_id = match &result {
                    ActorResult::Computed { node_id, .. } => *node_id,
                    ActorResult::Error { node_id, .. } => *node_id,
                };

                // Cache env and track handler nodes
                if let ActorResult::Computed { node_id, ref env, code_hash, ref result, ref preprocessed, .. } = result {
                    self.env_cache.insert(node_id, CachedEnv {
                        code_hash,
                        env: env.clone(),
                        preprocessed: preprocessed.clone(),
                    });
                    if result.has_message_handler {
                        self.handler_nodes.insert(node_id);
                    } else {
                        self.handler_nodes.remove(&node_id);
                    }
                }

                // Handle coalesce: if dirty, re-spawn with latest data
                let state = self.node_states.entry(node_id).or_insert(NodeState {
                    in_flight: false,
                    dirty: None,
                });
                state.in_flight = false;

                if let Some(pending) = state.dirty.take() {
                    // Re-spawn immediately (no debounce — data was already waiting)
                    self.spawn_eval(node_id, pending);
                    // Don't emit this result — a newer one is coming.
                    // But we still need to return something so poll loop continues.
                    // Return the current result — it'll be overwritten by the next one.
                }

                Some(result)
            }
            Err(_) => None,
        }
    }

    /// Access the shared engine (for register_node_library from main thread).
    pub fn engine(&self) -> &SchemeEngine {
        &self.engine
    }

    /// Actually spawn eval on a worker thread.
    fn spawn_eval(&mut self, node_id: NodeId, pending: PendingCompute) {
        let code_hash = hash_code(&pending.msg.node.script_code);
        let cached = self.env_cache.remove(&node_id)
            .filter(|c| c.code_hash == code_hash);
        let cached_env = cached.as_ref().map(|c| c.env.clone());
        let cached_preprocessed = cached.and_then(|c| c.preprocessed);

        let state = self.node_states.entry(node_id).or_insert(NodeState {
            in_flight: false,
            dirty: None,
        });
        state.in_flight = true;

        let engine = Arc::clone(&self.engine);
        let shared = self.shared.clone();
        let tx = self.result_tx.clone();

        self.pool.spawn(move || {
            let result = execute_on_thread(engine, node_id, pending.msg, &shared, cached_env, cached_preprocessed);
            let _ = tx.send(result);
            if let Some(ctx) = shared.egui_ctx.as_ref() {
                ctx.request_repaint();
            }
        });
    }
}

/// Execute any node type on a worker thread.
fn execute_on_thread(
    engine: Arc<SchemeEngine>,
    node_id: NodeId,
    msg: ActorMsg,
    shared: &SharedResources,
    cached_env: Option<TopLevelEnvironment>,
    cached_preprocessed: Option<String>,
) -> ActorResult {
    let builtin = msg.template.as_ref().and_then(|t| t.builtin);

    // Fast message path: drain messages, then re-eval with cached preprocessed code
    if msg.fast_message_path {
        if let Some(env) = cached_env {
            let mut ctx = ActorContext::new(node_id);
            ctx.net_values = shared.net_values.clone();
            ctx.session_mgr = shared.session_mgr.clone();
            ctx.ocapn_slots = shared.ocapn_slots.clone();
            ctx.connected_peers = shared.connected_peers.clone();
            ctx.ocapn_call_results = shared.ocapn_call_results.clone();
            ctx.node_mailboxes = shared.node_mailboxes.clone();
            ctx.slot_owners = shared.slot_owners.clone();

            let ch = hash_code(&msg.node.script_code);

            // Drain messages first
            let drain_result = with_actor_context(&mut ctx, || {
                engine.execute_message_handler(&env, &msg.available_inputs, Some(&msg.db))
            });

            if let Err(e) = drain_result {
                return ActorResult::Error { node_id, message: e.to_string() };
            }

            // Re-eval with cached preprocessed code for fresh render blocks
            if let Some(ref preprocessed) = cached_preprocessed {
                ctx.reset_outputs();
                let result = with_actor_context(&mut ctx, || {
                    engine.eval_preprocessed(&env, &msg.available_inputs, Some(&msg.db), preprocessed)
                });
                return match result {
                    Ok(script_result) => ActorResult::Computed {
                        node_id,
                        result: script_result,
                        env,
                        code_hash: ch,
                        preprocessed: cached_preprocessed,
                    },
                    Err(e) => ActorResult::Error { node_id, message: e.to_string() },
                };
            }

            // No cached preprocessed code — return drain-only result
            let drain_result = drain_result.unwrap();
            return ActorResult::Computed {
                node_id,
                result: drain_result,
                env,
                code_hash: ch,
                preprocessed: None,
            };
        }
    }

    match builtin {
        Some(BuiltinKind::Script) => {
            let code = &msg.node.script_code;
            if code.trim().is_empty() {
                return ActorResult::Computed {
                    node_id,
                    result: ScriptResult::empty(),
                    env: cached_env.unwrap_or_else(|| engine.make_env()),
                    code_hash: hash_code(code),
                    preprocessed: None,
                };
            }

            let mut ctx = ActorContext::new(node_id);
            ctx.net_values = shared.net_values.clone();
            ctx.session_mgr = shared.session_mgr.clone();
            ctx.ocapn_slots = shared.ocapn_slots.clone();
            ctx.connected_peers = shared.connected_peers.clone();
            ctx.ocapn_call_results = shared.ocapn_call_results.clone();
            ctx.node_mailboxes = shared.node_mailboxes.clone();
            ctx.slot_owners = shared.slot_owners.clone();

            let ch = hash_code(code);

            let result = with_actor_context(&mut ctx, || {
                engine.execute_script_cached(
                    cached_env, &msg.available_inputs, Some(&msg.db), code,
                    &msg.connected_modules,
                )
            });

            match result {
                Ok((script_result, env, preprocessed)) => ActorResult::Computed {
                    node_id,
                    result: script_result,
                    env,
                    code_hash: ch,
                    preprocessed: Some(preprocessed),
                },
                Err(e) => ActorResult::Error { node_id, message: e.to_string() },
            }
        }
        Some(BuiltinKind::Const) => {
            let output_values = crate::executor::execute_const(&msg.node);
            ActorResult::Computed {
                node_id,
                result: ScriptResult::with_outputs(output_values),
                env: cached_env.unwrap_or_else(|| engine.make_env()),
                code_hash: 0,
                preprocessed: None,
            }
        }
        Some(BuiltinKind::Output) => {
            let output_values = crate::executor::execute_output(&msg.available_inputs);
            ActorResult::Computed {
                node_id,
                result: ScriptResult::with_outputs(output_values),
                env: cached_env.unwrap_or_else(|| engine.make_env()),
                code_hash: 0,
                preprocessed: None,
            }
        }
        None => {
            // WASM node
            if let Some(ref wasm) = shared.wasm {
                if let Some(ref template) = msg.template {
                    match wasm.execute(template, &msg.available_inputs) {
                        Ok(output_values) => ActorResult::Computed {
                            node_id,
                            result: ScriptResult::with_outputs(output_values),
                            env: cached_env.unwrap_or_else(|| engine.make_env()),
                            code_hash: 0,
                            preprocessed: None,
                        },
                        Err(e) => ActorResult::Error { node_id, message: e.to_string() },
                    }
                } else {
                    ActorResult::Error { node_id, message: "No template for WASM node".into() }
                }
            } else {
                ActorResult::Error { node_id, message: "No WASM runner available".into() }
            }
        }
    }
}
