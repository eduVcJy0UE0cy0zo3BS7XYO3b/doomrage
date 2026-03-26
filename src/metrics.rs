//! Prometheus metrics for wasm-canvas.
//! All metrics are registered lazily on first access.

use prometheus::{
    Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
    TextEncoder, Encoder,
};
use std::sync::LazyLock;

pub static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);

// --- Compute ---

pub static COMPUTE_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    let h = Histogram::with_opts(HistogramOpts::new(
        "wasm_canvas_compute_duration_seconds",
        "Time to compute a single node",
    ).buckets(vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0]))
    .unwrap();
    REGISTRY.register(Box::new(h.clone())).unwrap();
    h
});

pub static COMPUTE_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("wasm_canvas_compute_total", "Total node computations").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static COMPUTE_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("wasm_canvas_compute_errors_total", "Total compute errors").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static PENDING_COMPUTES: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new("wasm_canvas_pending_computes", "Nodes waiting to compute").unwrap();
    REGISTRY.register(Box::new(g.clone())).unwrap();
    g
});

// --- Nodes ---

pub static NODES_TOTAL: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    let g = IntGaugeVec::new(
        Opts::new("wasm_canvas_nodes_total", "Number of nodes per canvas"),
        &["canvas"],
    ).unwrap();
    REGISTRY.register(Box::new(g.clone())).unwrap();
    g
});

pub static DEFINITIONS_TOTAL: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new("wasm_canvas_definitions_total", "Total definitions in Name DB").unwrap();
    REGISTRY.register(Box::new(g.clone())).unwrap();
    g
});

// --- nREPL ---

pub static NREPL_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    let c = IntCounterVec::new(
        Opts::new("wasm_canvas_nrepl_requests_total", "nREPL requests by operation"),
        &["op"],
    ).unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static NREPL_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    let h = Histogram::with_opts(HistogramOpts::new(
        "wasm_canvas_nrepl_duration_seconds",
        "nREPL request processing time",
    ).buckets(vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0]))
    .unwrap();
    REGISTRY.register(Box::new(h.clone())).unwrap();
    h
});

// --- P2P ---

pub static PEERS_CONNECTED: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new("wasm_canvas_peers_connected", "Connected P2P peers").unwrap();
    REGISTRY.register(Box::new(g.clone())).unwrap();
    g
});

pub static DEF_REQUESTS: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("wasm_canvas_def_requests_total", "Definition requests sent").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static DEF_RESPONSES_SERVED: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("wasm_canvas_def_responses_served_total", "Definitions served to peers").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static DEF_RESPONSES_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("wasm_canvas_def_responses_received_total", "Definitions received from peers").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static NETWORK_VALUES_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("wasm_canvas_network_values_received_total", "Value updates from network").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

// --- Database ---

pub static DB_QUERIES: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("wasm_canvas_db_queries_total", "Total SurrealDB queries").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static DB_QUERY_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    let h = Histogram::with_opts(HistogramOpts::new(
        "wasm_canvas_db_query_duration_seconds",
        "SurrealDB query duration",
    ).buckets(vec![0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1]))
    .unwrap();
    REGISTRY.register(Box::new(h.clone())).unwrap();
    h
});

pub static DB_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("wasm_canvas_db_errors_total", "SurrealDB query errors").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

// --- Memory ---

pub static MEMORY_RSS_BYTES: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new("wasm_canvas_memory_rss_bytes", "Resident set size (bytes)").unwrap();
    REGISTRY.register(Box::new(g.clone())).unwrap();
    g
});

pub static MEMORY_ALLOCATED_BYTES: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new("wasm_canvas_memory_allocated_bytes", "Allocator active bytes").unwrap();
    REGISTRY.register(Box::new(g.clone())).unwrap();
    g
});

pub static MEMORY_RESIDENT_BYTES: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new("wasm_canvas_memory_resident_bytes", "Allocator resident bytes").unwrap();
    REGISTRY.register(Box::new(g.clone())).unwrap();
    g
});

pub static ENV_CACHE_SIZE: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new("wasm_canvas_env_cache_size", "Cached Scheme environments").unwrap();
    REGISTRY.register(Box::new(g.clone())).unwrap();
    g
});

/// Read RSS from /proc/self/statm (Linux).
pub fn read_rss_bytes() -> i64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<i64>().ok())
        .map(|pages| pages * 4096)
        .unwrap_or(0)
}

// --- Gather ---

/// Render all metrics in Prometheus text format.
pub fn gather() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

/// Snapshot all metrics as a JSON object (one line for JSONL).
pub fn snapshot_json() -> String {
    use std::fmt::Write;
    let mut s = String::from("{");
    write!(s, "\"ts\":{:.3}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()).unwrap();
    write!(s, ",\"compute_total\":{}", COMPUTE_TOTAL.get()).unwrap();
    write!(s, ",\"compute_errors\":{}", COMPUTE_ERRORS.get()).unwrap();
    write!(s, ",\"pending_computes\":{}", PENDING_COMPUTES.get()).unwrap();
    write!(s, ",\"definitions_total\":{}", DEFINITIONS_TOTAL.get()).unwrap();
    write!(s, ",\"peers_connected\":{}", PEERS_CONNECTED.get()).unwrap();
    write!(s, ",\"def_requests\":{}", DEF_REQUESTS.get()).unwrap();
    write!(s, ",\"def_responses_served\":{}", DEF_RESPONSES_SERVED.get()).unwrap();
    write!(s, ",\"def_responses_received\":{}", DEF_RESPONSES_RECEIVED.get()).unwrap();
    write!(s, ",\"network_values_received\":{}", NETWORK_VALUES_RECEIVED.get()).unwrap();
    // Compute duration percentiles from histogram
    let h = COMPUTE_DURATION.get_sample_sum();
    let c = COMPUTE_DURATION.get_sample_count();
    write!(s, ",\"compute_duration_sum\":{:.6}", h).unwrap();
    write!(s, ",\"compute_duration_count\":{}", c).unwrap();
    if c > 0 {
        write!(s, ",\"compute_duration_avg_ms\":{:.3}", (h / c as f64) * 1000.0).unwrap();
    }
    // nREPL duration
    let nh = NREPL_DURATION.get_sample_sum();
    let nc = NREPL_DURATION.get_sample_count();
    write!(s, ",\"nrepl_duration_sum\":{:.6}", nh).unwrap();
    write!(s, ",\"nrepl_duration_count\":{}", nc).unwrap();
    if nc > 0 {
        write!(s, ",\"nrepl_duration_avg_ms\":{:.3}", (nh / nc as f64) * 1000.0).unwrap();
    }
    // DB
    write!(s, ",\"db_queries\":{}", DB_QUERIES.get()).unwrap();
    write!(s, ",\"db_errors\":{}", DB_ERRORS.get()).unwrap();
    let db_sum = DB_QUERY_DURATION.get_sample_sum();
    let db_count = DB_QUERY_DURATION.get_sample_count();
    write!(s, ",\"db_query_duration_sum\":{:.6}", db_sum).unwrap();
    write!(s, ",\"db_query_count\":{}", db_count).unwrap();
    if db_count > 0 {
        write!(s, ",\"db_query_avg_ms\":{:.3}", (db_sum / db_count as f64) * 1000.0).unwrap();
    }
    // Memory
    write!(s, ",\"memory_rss_bytes\":{}", MEMORY_RSS_BYTES.get()).unwrap();
    write!(s, ",\"memory_allocated_bytes\":{}", MEMORY_ALLOCATED_BYTES.get()).unwrap();
    write!(s, ",\"memory_resident_bytes\":{}", MEMORY_RESIDENT_BYTES.get()).unwrap();
    write!(s, ",\"env_cache_size\":{}", ENV_CACHE_SIZE.get()).unwrap();
    s.push('}');
    s
}

/// Update gauge metrics from runtime state.
pub fn update_gauges(runtime: &crate::graph_runtime::GraphRuntime) {
    // Node counts per canvas
    for (canvas, graph) in &runtime.all_graphs {
        NODES_TOTAL.with_label_values(&[canvas]).set(graph.nodes.len() as i64);
    }
    // Pending computes
    PENDING_COMPUTES.set(runtime.pending_nodes.len() as i64);
    // Definition count from DB
    let all_defs: i64 = runtime.all_graphs.keys()
        .map(|c| runtime.db.all_definitions(c).len() as i64)
        .sum();
    DEFINITIONS_TOTAL.set(all_defs);
    // Memory
    MEMORY_RSS_BYTES.set(read_rss_bytes());
    // Env cache size
    ENV_CACHE_SIZE.set(runtime.actor_runtime.env_cache_size() as i64);
}
