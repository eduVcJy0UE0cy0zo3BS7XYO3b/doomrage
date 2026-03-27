#!/bin/bash
set -e

# Load test with isolated containers in a pod:
#   Container 1 (peer):     fixed resources (4 CPU, 2GB)    — system under test
#   Container 2 (loadtest): fixed resources (2 CPU, 512MB)  — load generator
#   Both share network namespace via pod → localhost works
#
# Usage:
#   ./tests/loadtest/run.sh --soak-minutes 0              # find max only
#   ./tests/loadtest/run.sh --max-clients 5 --soak-minutes 60  # soak

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUTPUT_DIR="$ROOT_DIR/loadtest-output"

# Parse --tags out of args (not passed to loadtest binary)
TAGS=""
LOADTEST_ARGS=""
while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--tags" ]]; then
        shift
        TAGS="$1"
        shift
    else
        LOADTEST_ARGS="$LOADTEST_ARGS $1"
        shift
    fi
done
LOADTEST_ARGS="${LOADTEST_ARGS# }"
IMAGE="docker.io/library/ubuntu:24.04"
POD="loadtest-pod"
BUILD_DIR="$ROOT_DIR/target/debug"

RUN_ID=$(date +%Y%m%d-%H%M%S)
RUN_DIR="$OUTPUT_DIR/$RUN_ID"
mkdir -p "$RUN_DIR"

echo "=== Collecting hardware info ==="
python3 -c "
import json, subprocess, os, platform

hw = {}
hw['hostname'] = platform.node()
hw['kernel'] = platform.release()
hw['arch'] = platform.machine()
hw['python'] = platform.python_version()

# CPU
try:
    with open('/proc/cpuinfo') as f:
        cpuinfo = f.read()
    models = [l.split(':')[1].strip() for l in cpuinfo.split('\n') if 'model name' in l]
    hw['cpu_model'] = models[0] if models else 'unknown'
    hw['cpu_count'] = len(models)
except: hw['cpu_model'] = 'unknown'; hw['cpu_count'] = os.cpu_count()

# RAM
try:
    with open('/proc/meminfo') as f:
        for line in f:
            if 'MemTotal' in line:
                hw['ram_total_mb'] = int(line.split()[1]) // 1024
                break
except: pass

# Disk benchmark (sequential write 64MB)
try:
    import tempfile, time
    with tempfile.NamedTemporaryFile(dir='$RUN_DIR', delete=True) as f:
        data = b'x' * (64 * 1024 * 1024)
        t0 = time.monotonic()
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
        dt = time.monotonic() - t0
        hw['disk_seq_write_mb_s'] = round(64 / dt, 1)
except: pass

# Disk benchmark (sequential read)
try:
    import tempfile, time
    with tempfile.NamedTemporaryFile(dir='$RUN_DIR', delete=False) as f:
        fname = f.name
        f.write(b'x' * (64 * 1024 * 1024))
        f.flush()
        os.fsync(f.fileno())
    # Drop page cache for this file if possible
    os.system(f'dd if={fname} of=/dev/null bs=1M 2>/dev/null')
    t0 = time.monotonic()
    with open(fname, 'rb') as f:
        f.read()
    dt = time.monotonic() - t0
    hw['disk_seq_read_mb_s'] = round(64 / dt, 1)
    os.unlink(fname)
except: pass

# Container limits
hw['container_cpus'] = 4
hw['container_memory_mb'] = 2048

# Rust version
try:
    r = subprocess.run(['rustc', '--version'], capture_output=True, text=True)
    hw['rustc'] = r.stdout.strip()
except: pass

json.dump(hw, open('$RUN_DIR/hw-info.json', 'w'), indent=2)
for k, v in sorted(hw.items()):
    print(f'  {k}: {v}')
" 2>/dev/null || echo "  (hw-info collection failed)"

echo ""
echo "=== Building binaries ==="
cargo build -p wasm-canvas-peer -p canvas-loadtest 2>&1 | tail -3

# Clean up previous
podman pod rm -f "$POD" 2>/dev/null || true

echo ""
echo "=== Creating pod ==="
podman pod create --name "$POD" -p 9099:9090

echo ""
echo "=== Starting peer (4 CPUs, 2GB RAM) ==="
podman run -d --rm \
    --pod "$POD" \
    --name "${POD}-peer" \
    --cpus=4 --memory=2g \
    -v "$BUILD_DIR/wasm-canvas-peer:/usr/local/bin/peer:ro" \
    -v "$RUN_DIR:/output" \
    -e RUST_LOG=warn,wasm_canvas=info \
    "$IMAGE" \
    /bin/bash -c '
        PROJECT=$(mktemp -d)
        peer --init $PROJECT >/dev/null 2>&1
        # Symlink metrics to shared volume so they are always accessible
        ln -sf /output/peer-metrics.jsonl $PROJECT/.canvas/metrics.jsonl 2>/dev/null || true
        peer --project $PROJECT &
        PEER_PID=$!
        wait $PEER_PID
    '

echo "Waiting for peer..."
for i in $(seq 1 30); do
    if curl -s localhost:9099/metrics >/dev/null 2>&1; then
        echo "Peer ready"
        break
    fi
    sleep 1
done

echo ""
echo "=== Starting host metrics collector ==="
python3 "$ROOT_DIR/tools/host-metrics-collector.py" "$RUN_DIR" --pod "$POD" &
HM_PID=$!

echo ""
echo "=== Running loadtest (2 CPUs, 512MB RAM, same pod network) ==="
podman run --rm \
    --pod "$POD" \
    --name "${POD}-load" \
    --cpus=2 --memory=512m \
    -v "$BUILD_DIR/canvas-loadtest:/usr/local/bin/loadtest:ro" \
    -v "$RUN_DIR:/output" \
    "$IMAGE" \
    /bin/bash -c "cd /output && loadtest --addr 127.0.0.1:7888 ${LOADTEST_ARGS}"
EXIT_CODE=$?

# Peer metrics are written directly to volume via symlink
echo ""
if [ -s "$RUN_DIR/peer-metrics.jsonl" ]; then
    echo "=== Peer metrics: $(wc -l < "$RUN_DIR/peer-metrics.jsonl") samples ==="
else
    echo "=== No peer metrics (peer may not have started) ==="
fi

echo ""
echo "=== Stopping host metrics collector ==="
kill "$HM_PID" 2>/dev/null
wait "$HM_PID" 2>/dev/null

echo ""
echo "=== Generating report ==="
echo "$LOADTEST_ARGS" > "$RUN_DIR/args.txt"

# Write meta.json with tags
python3 -c "
import json, os
meta_path = '$RUN_DIR/meta.json'
meta = {}
if os.path.exists(meta_path):
    try: meta = json.load(open(meta_path))
    except: pass
tags_str = '''$TAGS'''.strip()
meta['tags'] = [t.strip() for t in tags_str.split(',') if t.strip()] if tags_str else meta.get('tags', [])
json.dump(meta, open(meta_path, 'w'), indent=2)
"

python3 "$ROOT_DIR/tools/metrics-report.py" "$RUN_DIR/loadtest-results.jsonl" -o "$RUN_DIR/report.html" 2>/dev/null && \
    echo "Report: $RUN_DIR/report.html" || echo "(no report data)"

echo ""
echo "=== Cleanup ==="
podman pod stop "$POD" 2>/dev/null || true
podman pod rm -f "$POD" 2>/dev/null || true

# Generate index of all runs
python3 "$ROOT_DIR/tools/metrics-report.py" index "$OUTPUT_DIR" 2>/dev/null

echo ""
echo "Run:      $RUN_DIR"
echo "Index:    $OUTPUT_DIR/index.html"
ls -lh "$RUN_DIR/"
exit $EXIT_CODE
