#!/bin/bash
set -e

# Load test with isolated containers in a pod:
#   Container 1 (peer): fixed resources (4 CPU, 2GB) — system under test
#   Container 2 (loadtest): no limits — load generator
#   Both share network namespace via pod → localhost works
#
# Usage:
#   ./tests/loadtest/run.sh --soak-minutes 0              # find max only
#   ./tests/loadtest/run.sh --max-clients 5 --soak-minutes 60  # soak

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOADTEST_ARGS="${@}"
OUTPUT_DIR="$ROOT_DIR/loadtest-output"
IMAGE="docker.io/library/ubuntu:24.04"
POD="loadtest-pod"
BUILD_DIR="$ROOT_DIR/target/debug"

RUN_ID=$(date +%Y%m%d-%H%M%S)
RUN_DIR="$OUTPUT_DIR/$RUN_ID"
mkdir -p "$RUN_DIR"

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
        peer --project $PROJECT &
        PEER_PID=$!
        trap "cp $PROJECT/.canvas/metrics.jsonl /output/peer-metrics.jsonl 2>/dev/null; kill $PEER_PID" EXIT
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
echo "=== Running loadtest (no resource limits, same pod network) ==="
podman run --rm \
    --pod "$POD" \
    --name "${POD}-load" \
    -v "$BUILD_DIR/canvas-loadtest:/usr/local/bin/loadtest:ro" \
    -v "$RUN_DIR:/output" \
    "$IMAGE" \
    /bin/bash -c "cd /output && loadtest --addr 127.0.0.1:7888 ${LOADTEST_ARGS}"
EXIT_CODE=$?

echo ""
echo "=== Generating report ==="
echo "$LOADTEST_ARGS" > "$RUN_DIR/args.txt"
python3 "$ROOT_DIR/tools/metrics-report.py" "$RUN_DIR/loadtest-results.jsonl" -o "$RUN_DIR/report.html" 2>/dev/null && \
    echo "Report: $RUN_DIR/report.html" || echo "(no report data)"

echo ""
echo "=== Cleanup ==="
podman pod stop "$POD" 2>/dev/null || true
podman pod rm -f "$POD" 2>/dev/null || true

# Generate index of all runs
python3 -c "
import os, json
runs_dir = '$OUTPUT_DIR'
runs = sorted([d for d in os.listdir(runs_dir) if os.path.isdir(os.path.join(runs_dir, d))], reverse=True)
html = '<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Load Test History</title>'
html += '<style>body{font-family:sans-serif;max-width:900px;margin:40px auto;padding:0 20px}'
html += 'table{width:100%;border-collapse:collapse}td,th{padding:10px;border:1px solid #ddd;text-align:left}'
html += 'tr:hover{background:#f5f5f5}a{color:#2266cc}</style></head><body>'
html += '<h1>Load Test History</h1><table><tr><th>Date</th><th>Args</th><th>Summary</th><th>Report</th></tr>'
for run in runs:
    rd = os.path.join(runs_dir, run)
    args = open(os.path.join(rd, 'args.txt')).read().strip() if os.path.exists(os.path.join(rd, 'args.txt')) else ''
    summary = ''
    jf = os.path.join(rd, 'loadtest-results.jsonl')
    if os.path.exists(jf):
        lines = open(jf).readlines()
        if lines:
            try:
                last = json.loads(lines[-1])
                summary = 'p99={:.0f}ms err={:.1f}% ops={}'.format(last.get('p99_ms',0), last.get('error_rate',0), last.get('ops',0))
            except: pass
    link = '<a href=\"{}/report.html\">view</a>'.format(run) if os.path.exists(os.path.join(rd, 'report.html')) else '-'
    html += '<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>'.format(run, args, summary, link)
html += '</table></body></html>'
open(os.path.join(runs_dir, 'index.html'), 'w').write(html)
print('Index: {}/index.html'.format(runs_dir))
" 2>/dev/null

echo ""
echo "Run:      $RUN_DIR"
echo "Index:    $OUTPUT_DIR/index.html"
ls -lh "$RUN_DIR/"
exit $EXIT_CODE
