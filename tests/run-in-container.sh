#!/bin/bash
set -e

# Full-cycle e2e test in a container sandbox.
# Agent connects directly to mock LLM (no litellm dependency for tests).
# For production: use LLM_API_BASE=http://localhost:4000 with litellm.
#
# Usage: ./tests/run-in-container.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== Building binaries ==="
if [ -f "$ROOT_DIR/target/release/wasm-canvas-peer" ] && [ -f "$ROOT_DIR/target/release/canvas-agent" ]; then
    BUILD_DIR="$ROOT_DIR/target/release"
    echo "Using existing release binaries"
else
    cargo build -p wasm-canvas-peer -p canvas-agent 2>&1 | tail -3
    BUILD_DIR="$ROOT_DIR/target/debug"
fi

echo ""
echo "=== Starting mock LLM ==="
podman rm -f mock-llm 2>/dev/null || true
podman run -d --rm --name mock-llm \
    -p 9999:9999 \
    -v "$ROOT_DIR/mock-llm:/app:ro" \
    -w /app \
    python:3.12-slim \
    python3 server.py 9999
sleep 2
echo "Mock LLM on :9999"

echo ""
echo "=== Building test container ==="
podman build -t canvas-test -f "$SCRIPT_DIR/Containerfile" "$SCRIPT_DIR" 2>&1 | tail -3

echo ""
echo "=== Running test in container ==="
podman run --rm \
    --network=host \
    -v "$BUILD_DIR/wasm-canvas-peer:/usr/local/bin/peer:ro" \
    -v "$BUILD_DIR/canvas-agent:/usr/local/bin/canvas-agent:ro" \
    -v "$SCRIPT_DIR/full-cycle-container.sh:/opt/test.sh:ro" \
    canvas-test bash /opt/test.sh
EXIT_CODE=$?

echo ""
echo "=== Cleanup ==="
podman rm -f mock-llm 2>/dev/null || true

exit $EXIT_CODE
