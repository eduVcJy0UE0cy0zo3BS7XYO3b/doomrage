#!/bin/bash
set -e

# Load test in a container with fixed resources.
#
# Usage:
#   ./tests/loadtest/run.sh                    # defaults: 1 hour soak
#   ./tests/loadtest/run.sh --soak-minutes 5   # quick 5-min test
#   ./tests/loadtest/run.sh --max-clients 10   # skip ramp-up, go straight to 10

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SOAK_ARGS="${@}"

echo "=== Building binaries ==="
cargo build -p wasm-canvas-peer -p canvas-loadtest 2>&1 | tail -3
BUILD_DIR="$ROOT_DIR/target/debug"

echo ""
echo "=== Building container ==="
podman build -t canvas-loadtest -f - "$ROOT_DIR" <<'DOCKERFILE'
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 python3 ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace
DOCKERFILE

echo ""
echo "=== Running load test (4 CPUs, 2GB RAM) ==="
podman run --rm \
    --cpus=4 --memory=2g \
    --network=host \
    -v "$BUILD_DIR/wasm-canvas-peer:/usr/local/bin/peer:ro" \
    -v "$BUILD_DIR/canvas-loadtest:/usr/local/bin/loadtest:ro" \
    -v "$ROOT_DIR/tools/metrics-report.py:/usr/local/bin/metrics-report:ro" \
    -v "$(pwd)/loadtest-output:/output" \
    -e RUST_LOG=warn,wasm_canvas=info \
    bash -c "
        set -e

        # Init project
        PROJECT=\$(mktemp -d)
        peer --init \$PROJECT >/dev/null 2>&1

        # Start peer
        peer --project \$PROJECT &
        PEER_PID=\$!
        sleep 3

        if ! kill -0 \$PEER_PID 2>/dev/null; then
            echo 'FAIL: peer did not start'
            exit 1
        fi

        echo 'Peer started (PID '\$PEER_PID')'

        # Run load test
        cd /output
        loadtest --addr 127.0.0.1:7888 ${SOAK_ARGS}
        EXIT_CODE=\$?

        # Copy metrics
        cp \$PROJECT/.canvas/metrics.jsonl /output/peer-metrics.jsonl 2>/dev/null || true

        # Generate report
        python3 /usr/local/bin/metrics-report /output/loadtest-results.jsonl -o /output/loadtest-report.html 2>/dev/null || true

        # Stop peer
        kill \$PEER_PID 2>/dev/null || true
        wait \$PEER_PID 2>/dev/null || true

        exit \$EXIT_CODE
    "

EXIT_CODE=$?

echo ""
echo "=== Output ==="
ls -la loadtest-output/ 2>/dev/null || echo "(no output dir)"
echo ""
if [ -f loadtest-output/loadtest-report.html ]; then
    echo "Report: loadtest-output/loadtest-report.html"
    echo "Data:   loadtest-output/loadtest-results.jsonl"
    xdg-open loadtest-output/loadtest-report.html 2>/dev/null || true
fi

exit $EXIT_CODE
