//! Load test for wasm-canvas peer via nREPL.
//!
//! Phase 1 (ramp-up): starts 1 client, adds +1 every 30s until p99 > 1s or errors > 1%.
//! Phase 2 (soak): holds max clients for --soak-minutes (default 60).
//!
//! Each client runs a realistic nREPL session cycle:
//!   create-node → update-node → compute → node-state → defs → info → rename → delete
//!
//! Usage:
//!   canvas-loadtest --addr 127.0.0.1:7888 --ramp-step 30 --soak-minutes 60

use nrepl::{Client, bencode::Value};

macro_rules! timed {
    ($metrics:expr, $block:expr) => {{
        let _start = Instant::now();
        let _ok = $block;
        $metrics.record(_start.elapsed(), !_ok);
        _ok
    }};
}
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::{Duration, Instant};
use std::thread;

// --- Config ---

struct Config {
    addr: String,
    ramp_step_secs: u64,
    soak_minutes: u64,
    max_clients: usize, // 0 = auto-detect
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let mut cfg = Config {
        addr: "127.0.0.1:7888".into(),
        ramp_step_secs: 30,
        soak_minutes: 60,
        max_clients: 0,
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => { cfg.addr = args[i + 1].clone(); i += 2; }
            "--ramp-step" => { cfg.ramp_step_secs = args[i + 1].parse().unwrap(); i += 2; }
            "--soak-minutes" => { cfg.soak_minutes = args[i + 1].parse().unwrap(); i += 2; }
            "--max-clients" => { cfg.max_clients = args[i + 1].parse().unwrap(); i += 2; }
            _ => { i += 1; }
        }
    }
    cfg
}

// --- Metrics ---

struct Metrics {
    latencies_us: Mutex<VecDeque<u64>>, // rolling window of latencies in microseconds
    total_ops: AtomicU64,
    total_errors: AtomicU64,
    total_cycles: AtomicU64,
}

impl Metrics {
    fn new() -> Self {
        Self {
            latencies_us: Mutex::new(VecDeque::new()),
            total_ops: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            total_cycles: AtomicU64::new(0),
        }
    }

    fn record(&self, latency: Duration, is_error: bool) {
        let us = latency.as_micros() as u64;
        let mut lat = self.latencies_us.lock().unwrap();
        lat.push_back(us);
        // Keep last 10000 samples
        while lat.len() > 10000 { lat.pop_front(); }
        drop(lat);
        self.total_ops.fetch_add(1, Ordering::Relaxed);
        if is_error {
            self.total_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_cycle(&self) {
        self.total_cycles.fetch_add(1, Ordering::Relaxed);
    }

    fn p99_ms(&self) -> f64 {
        let lat = self.latencies_us.lock().unwrap();
        if lat.is_empty() { return 0.0; }
        let mut sorted: Vec<u64> = lat.iter().copied().collect();
        sorted.sort();
        let idx = (sorted.len() as f64 * 0.99) as usize;
        let idx = idx.min(sorted.len() - 1);
        sorted[idx] as f64 / 1000.0
    }

    fn p50_ms(&self) -> f64 {
        let lat = self.latencies_us.lock().unwrap();
        if lat.is_empty() { return 0.0; }
        let mut sorted: Vec<u64> = lat.iter().copied().collect();
        sorted.sort();
        sorted[sorted.len() / 2] as f64 / 1000.0
    }

    fn error_rate(&self) -> f64 {
        let total = self.total_ops.load(Ordering::Relaxed);
        if total == 0 { return 0.0; }
        self.total_errors.load(Ordering::Relaxed) as f64 / total as f64 * 100.0
    }

    fn ops(&self) -> u64 { self.total_ops.load(Ordering::Relaxed) }
    fn errors(&self) -> u64 { self.total_errors.load(Ordering::Relaxed) }
    fn cycles(&self) -> u64 { self.total_cycles.load(Ordering::Relaxed) }
}

// --- JSONL writer ---

struct JsonlWriter {
    file: std::fs::File,
    start: Instant,
}

impl JsonlWriter {
    fn new(path: &str) -> Self {
        Self {
            file: std::fs::File::create(path).expect("cannot create JSONL"),
            start: Instant::now(),
        }
    }

    fn write(&mut self, metrics: &Metrics, clients: usize, phase: &str) {
        use std::io::Write;
        let elapsed = self.start.elapsed().as_secs_f64();
        let line = format!(
            "{{\"t\":{:.1},\"phase\":\"{}\",\"clients\":{},\"ops\":{},\"errors\":{},\"cycles\":{},\"p50_ms\":{:.1},\"p99_ms\":{:.1},\"error_rate\":{:.2}}}",
            elapsed, phase, clients,
            metrics.ops(), metrics.errors(), metrics.cycles(),
            metrics.p50_ms(), metrics.p99_ms(), metrics.error_rate()
        );
        let _ = writeln!(self.file, "{}", line);
        let _ = self.file.flush();
    }
}

// --- Client worker ---

fn client_worker(
    id: usize,
    addr: String,
    metrics: Arc<Metrics>,
    stop: Arc<AtomicBool>,
) {
    let canvas = "default";

    // Connect
    let mut client = match Client::connect(&addr) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[client-{}] connect failed: {}", id, e);
            return;
        }
    };
    let session = match client.clone_session() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[client-{}] clone_session failed: {}", id, e);
            return;
        }
    };

    let label = format!("load-{}-{}", id, std::process::id());
    let mut cycle = 0u64;

    while !stop.load(Ordering::Relaxed) {
        cycle += 1;
        let node_label = format!("{}-{}", label, cycle);

        // 1. create-node
        let ok = timed!(metrics, {
            let id = next_id_static();
            client.send(&Value::dict(vec![
                ("id", Value::string(&id)),
                ("op", Value::string("create-node")),
                ("session", Value::string(&session)),
                ("canvas", Value::string(canvas)),
                ("label", Value::string(&node_label)),
                ("code", Value::string("(define x 1)")),
                ("exports", Value::List(vec![Value::string("x")])),
            ])).ok();
            client.recv_until_done().is_ok()
        });
        if !ok { continue; }

        // 2. update-node with more complex code
        timed!(metrics, {
            let id = next_id_static();
            client.send(&Value::dict(vec![
                ("id", Value::string(&id)),
                ("op", Value::string("update-node")),
                ("session", Value::string(&session)),
                ("canvas", Value::string(canvas)),
                ("label", Value::string(&node_label)),
                ("code", Value::string(&format!("(define x (* {} 2))\n(define y (+ x 1))", cycle))),
                ("exports", Value::List(vec![Value::string("x"), Value::string("y")])),
            ])).ok();
            client.recv_until_done().is_ok()
        });

        // 3. compute
        timed!(metrics, {
            let id = next_id_static();
            client.send(&Value::dict(vec![
                ("id", Value::string(&id)),
                ("op", Value::string("compute")),
                ("session", Value::string(&session)),
                ("canvas", Value::string(canvas)),
                ("label", Value::string(&node_label)),
            ])).ok();
            client.recv_until_done().is_ok()
        });

        // 4. wait for async compute
        thread::sleep(Duration::from_millis(300));

        // 5. node-state
        timed!(metrics, {
            let id = next_id_static();
            client.send(&Value::dict(vec![
                ("id", Value::string(&id)),
                ("op", Value::string("node-state")),
                ("session", Value::string(&session)),
                ("canvas", Value::string(canvas)),
                ("label", Value::string(&node_label)),
            ])).ok();
            client.recv_until_done().is_ok()
        });

        // 6. defs
        timed!(metrics, {
            let id = next_id_static();
            client.send(&Value::dict(vec![
                ("id", Value::string(&id)),
                ("op", Value::string("defs")),
                ("session", Value::string(&session)),
                ("canvas", Value::string(canvas)),
            ])).ok();
            client.recv_until_done().is_ok()
        });

        // 7. info
        timed!(metrics, {
            let id = next_id_static();
            client.send(&Value::dict(vec![
                ("id", Value::string(&id)),
                ("op", Value::string("info")),
                ("session", Value::string(&session)),
                ("symbol", Value::string("x")),
            ])).ok();
            client.recv_until_done().is_ok()
        });

        // 8. delete-node
        timed!(metrics, {
            let id = next_id_static();
            client.send(&Value::dict(vec![
                ("id", Value::string(&id)),
                ("op", Value::string("delete-node")),
                ("session", Value::string(&session)),
                ("canvas", Value::string(canvas)),
                ("label", Value::string(&node_label)),
            ])).ok();
            client.recv_until_done().is_ok()
        });

        metrics.record_cycle();
    }
}

static OP_COUNTER: AtomicU64 = AtomicU64::new(0);
fn next_id_static() -> String {
    format!("load-{}", OP_COUNTER.fetch_add(1, Ordering::Relaxed))
}

// --- Main ---

fn main() {
    let cfg = parse_args();
    let metrics = Arc::new(Metrics::new());
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut jsonl = JsonlWriter::new("loadtest-results.jsonl");

    println!("=== wasm-canvas load test ===");
    println!("Target: {}", cfg.addr);
    println!("Ramp step: {}s", cfg.ramp_step_secs);
    println!("Soak: {} min", cfg.soak_minutes);
    println!();

    // --- Phase 1: Ramp-up ---
    println!("--- Phase 1: Ramp-up ---");
    let mut num_clients = 0usize;
    let mut max_clients = 0usize;
    let ramp_start = Instant::now();

    loop {
        num_clients += 1;

        if cfg.max_clients > 0 && num_clients > cfg.max_clients {
            max_clients = cfg.max_clients;
            println!("  Reached configured max: {} clients", max_clients);
            break;
        }

        // Spawn new client
        let addr = cfg.addr.clone();
        let m = metrics.clone();
        let s = stop.clone();
        handles.push(thread::spawn(move || client_worker(num_clients, addr, m, s)));

        println!("  Clients: {} | waiting {}s...", num_clients, cfg.ramp_step_secs);

        // Wait ramp step, printing stats
        let step_start = Instant::now();
        while step_start.elapsed() < Duration::from_secs(cfg.ramp_step_secs) {
            thread::sleep(Duration::from_secs(2));
            jsonl.write(&metrics, num_clients, "ramp");

            let p99 = metrics.p99_ms();
            let err = metrics.error_rate();
            print!("\r    p50={:.0}ms p99={:.0}ms err={:.1}% ops={} cycles={}    ",
                   metrics.p50_ms(), p99, err, metrics.ops(), metrics.cycles());

            // Check limits
            if metrics.ops() > 100 && (p99 > 1000.0 || err > 1.0) {
                max_clients = if num_clients > 1 { num_clients - 1 } else { 1 };
                println!();
                println!("  LIMIT HIT at {} clients (p99={:.0}ms err={:.1}%)", num_clients, p99, err);
                println!("  Max sustainable: {} clients", max_clients);
                // Kill the last client
                stop.store(true, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(500));
                stop.store(false, Ordering::Relaxed);
                break;
            }
        }
        println!();

        if max_clients > 0 { break; }

        // Safety: don't go above 50 clients
        if num_clients >= 50 {
            max_clients = 50;
            println!("  Reached safety limit: 50 clients");
            break;
        }
    }

    if max_clients == 0 { max_clients = num_clients; }

    let ramp_duration = ramp_start.elapsed();
    println!();
    println!("Ramp-up done in {:.0}s. Max clients: {}", ramp_duration.as_secs_f64(), max_clients);

    // Stop all current clients
    stop.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_secs(1));
    for h in handles.drain(..) { let _ = h.join(); }
    stop.store(false, Ordering::Relaxed);

    // Reset metrics for soak
    // (counters keep going, that's fine — we track from snapshot)

    // --- Phase 2: Soak ---
    println!();
    println!("--- Phase 2: Soak ({} min, {} clients) ---", cfg.soak_minutes, max_clients);

    let soak_metrics = Arc::new(Metrics::new());

    for i in 1..=max_clients {
        let addr = cfg.addr.clone();
        let m = soak_metrics.clone();
        let s = stop.clone();
        handles.push(thread::spawn(move || client_worker(i, addr, m, s)));
    }

    let soak_start = Instant::now();
    let soak_duration = Duration::from_secs(cfg.soak_minutes * 60);
    let mut last_print = Instant::now();

    while soak_start.elapsed() < soak_duration {
        thread::sleep(Duration::from_secs(2));
        jsonl.write(&soak_metrics, max_clients, "soak");

        if last_print.elapsed() > Duration::from_secs(10) {
            let elapsed = soak_start.elapsed();
            let remaining = if soak_duration > elapsed { soak_duration - elapsed } else { Duration::ZERO };
            println!(
                "  [{:>5.0}s remaining] clients={} ops={} cycles={} p50={:.0}ms p99={:.0}ms err={:.1}%",
                remaining.as_secs_f64(), max_clients,
                soak_metrics.ops(), soak_metrics.cycles(),
                soak_metrics.p50_ms(), soak_metrics.p99_ms(), soak_metrics.error_rate()
            );
            last_print = Instant::now();
        }
    }

    // Stop
    stop.store(true, Ordering::Relaxed);
    println!();
    println!("--- Soak complete ---");

    for h in handles.drain(..) { let _ = h.join(); }

    // --- Summary ---
    println!();
    println!("=== Results ===");
    println!("Max clients:     {}", max_clients);
    println!("Total ops:       {}", soak_metrics.ops());
    println!("Total cycles:    {}", soak_metrics.cycles());
    println!("Total errors:    {}", soak_metrics.errors());
    println!("Error rate:      {:.2}%", soak_metrics.error_rate());
    println!("p50 latency:     {:.1}ms", soak_metrics.p50_ms());
    println!("p99 latency:     {:.1}ms", soak_metrics.p99_ms());
    let throughput = soak_metrics.cycles() as f64 / soak_duration.as_secs_f64();
    println!("Throughput:      {:.1} cycles/sec", throughput);
    println!();
    println!("Results written to: loadtest-results.jsonl");
    println!("Generate report:    python3 tools/metrics-report.py loadtest-results.jsonl -o loadtest.html");

    // Exit code
    if soak_metrics.error_rate() > 1.0 || soak_metrics.p99_ms() > 1000.0 {
        println!();
        println!("FAIL: system did not sustain load");
        std::process::exit(1);
    } else {
        println!();
        println!("PASS: system sustained {} clients for {} min", max_clients, cfg.soak_minutes);
    }
}
