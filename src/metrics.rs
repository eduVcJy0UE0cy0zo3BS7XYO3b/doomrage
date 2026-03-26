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

// --- Gather ---

/// Render all metrics in Prometheus text format.
pub fn gather() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
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
}
