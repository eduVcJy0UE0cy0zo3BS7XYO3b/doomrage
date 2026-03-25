#!/bin/bash
set -e

# Build binaries on host, run full-cycle test in a container (sandbox).
# Requires: podman, litellm running on localhost:4000, litellm-net network.
#
# Usage: ./tests/run-in-container.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== Building binaries ==="
if [ -f "$ROOT_DIR/target/release/wasm-canvas-peer" ] && [ -f "$ROOT_DIR/target/release/canvas-agent" ]; then
    BUILD_DIR="$ROOT_DIR/target/release"
    echo "Using existing release binaries"
else
    cargo build -p wasm-canvas-peer -p canvas-agent -p nrepl 2>&1 | tail -3
    BUILD_DIR="$ROOT_DIR/target/debug"
fi

echo ""
echo "=== Starting mock LLM container ==="
podman rm -f mock-llm 2>/dev/null || true
podman run -d --rm --name mock-llm \
    --network litellm-net \
    -v "$ROOT_DIR/mock-llm:/app:ro" \
    -w /app \
    python:3.12-slim \
    python3 server.py 9999
sleep 3
# Restart litellm to pick up mock-llm container in the network
podman restart litellm
sleep 5

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
