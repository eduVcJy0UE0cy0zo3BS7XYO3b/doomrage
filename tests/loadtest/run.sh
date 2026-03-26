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

mkdir -p "$OUTPUT_DIR"

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
    -v "$OUTPUT_DIR:/output" \
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
    -v "$OUTPUT_DIR:/output" \
    "$IMAGE" \
    /bin/bash -c "cd /output && loadtest --addr 127.0.0.1:7888 ${LOADTEST_ARGS}"
EXIT_CODE=$?

echo ""
echo "=== Generating report ==="
python3 "$ROOT_DIR/tools/metrics-report.py" "$OUTPUT_DIR/loadtest-results.jsonl" -o "$OUTPUT_DIR/loadtest-report.html" 2>/dev/null && \
    echo "Report: $OUTPUT_DIR/loadtest-report.html" || echo "(no report data)"

echo ""
echo "=== Cleanup ==="
podman pod stop "$POD" 2>/dev/null || true
podman pod rm -f "$POD" 2>/dev/null || true

echo ""
ls -lh "$OUTPUT_DIR/"*.jsonl "$OUTPUT_DIR/"*.html 2>/dev/null
exit $EXIT_CODE
