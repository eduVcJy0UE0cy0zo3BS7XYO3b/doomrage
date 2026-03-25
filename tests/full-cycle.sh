#!/bin/bash
set -e

# Full cycle test: init → peer → mock → agent → verify
# Usage: ./tests/full-cycle.sh

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PROJECT_DIR=$(mktemp -d)
MOCK_PID=""
PEER_PID=""

cleanup() {
    echo "[test] Cleaning up..."
    [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null || true
    [ -n "$PEER_PID" ] && kill "$PEER_PID" 2>/dev/null || true
    rm -rf "$PROJECT_DIR"
}
trap cleanup EXIT

PASS=0
FAIL=0

assert() {
    local desc="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        echo "  OK: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc"
        FAIL=$((FAIL + 1))
    fi
}

assert_file_contains() {
    local desc="$1"
    local file="$2"
    local pattern="$3"
    if grep -q "$pattern" "$file" 2>/dev/null; then
        echo "  OK: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (pattern '$pattern' not in $file)"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== Full cycle test ==="
echo "[test] Project dir: $PROJECT_DIR"

# --- Step 1: Init ---
echo ""
echo "--- Step 1: Init project ---"
cargo run -p wasm-canvas-peer -- --init "$PROJECT_DIR" 2>&1 | tail -1

assert "project dir exists" test -d "$PROJECT_DIR"
assert ".canvas dir exists" test -d "$PROJECT_DIR/.canvas"
assert "nodes/main dir exists" test -d "$PROJECT_DIR/.canvas/nodes/main"
assert "db.json exists" test -f "$PROJECT_DIR/.canvas/db.json"
assert ".env exists" test -f "$PROJECT_DIR/.env"
assert ".gitignore exists" test -f "$PROJECT_DIR/.gitignore"
assert_file_contains ".env has LLM_API_BASE" "$PROJECT_DIR/.env" "LLM_API_BASE"
assert_file_contains ".env has LLM_MODEL" "$PROJECT_DIR/.env" "LLM_MODEL"
assert_file_contains ".env has LLM_API_KEY" "$PROJECT_DIR/.env" "LLM_API_KEY"
assert_file_contains ".gitignore has .env" "$PROJECT_DIR/.gitignore" ".env"

# --- Step 2: Start mock LLM ---
echo ""
echo "--- Step 2: Start mock LLM ---"
python3 "$SCRIPT_DIR/mock-llm/server.py" &
MOCK_PID=$!
sleep 1
assert "mock server running" kill -0 "$MOCK_PID"

# --- Step 3: Start peer ---
echo ""
echo "--- Step 3: Start peer ---"
cargo run -p wasm-canvas-peer -- --project "$PROJECT_DIR" > /tmp/peer-test.log 2>&1 &
PEER_PID=$!
sleep 3
assert "peer running" kill -0 "$PEER_PID"
assert ".nrepl-port exists" test -f "$PROJECT_DIR/.canvas/.nrepl-port"

NREPL_PORT=$(cat "$PROJECT_DIR/.canvas/.nrepl-port")
echo "  nREPL port: $NREPL_PORT"

# --- Step 4: Run agent ---
echo ""
echo "--- Step 4: Run agent ---"
LLM_API_BASE=http://localhost:9999 LLM_MODEL=mock-agent LLM_API_KEY=fake \
    cargo run -p canvas-agent -- --project "$PROJECT_DIR" "Напиши осциллограф" 2>&1 | tail -5

# Give peer time to process commands
sleep 2

# --- Step 5: Verify results ---
echo ""
echo "--- Step 5: Verify results ---"

# Canvas directory created
assert "oscillator canvas dir exists" test -d "$PROJECT_DIR/.canvas/nodes/oscillator"

# Node files exist
assert "controls.scm exists" test -f "$PROJECT_DIR/.canvas/nodes/oscillator/controls.scm"
assert "wave.scm exists" test -f "$PROJECT_DIR/.canvas/nodes/oscillator/wave.scm"

# File contents
assert_file_contains "controls has widget gain" \
    "$PROJECT_DIR/.canvas/nodes/oscillator/controls.scm" "widget.*gain"
assert_file_contains "controls has widget freq" \
    "$PROJECT_DIR/.canvas/nodes/oscillator/controls.scm" "widget.*freq"
assert_file_contains "wave has canvas drawing" \
    "$PROJECT_DIR/.canvas/nodes/oscillator/wave.scm" "canvas"
assert_file_contains "wave has draw-polyline" \
    "$PROJECT_DIR/.canvas/nodes/oscillator/wave.scm" "draw-polyline"
assert_file_contains "wave has wave-points function" \
    "$PROJECT_DIR/.canvas/nodes/oscillator/wave.scm" "wave-points"

# Files are non-empty
assert "controls.scm not empty" test -s "$PROJECT_DIR/.canvas/nodes/oscillator/controls.scm"
assert "wave.scm not empty" test -s "$PROJECT_DIR/.canvas/nodes/oscillator/wave.scm"

# File count check
echo ""
echo "--- Step 6: Verify node count ---"
NODE_COUNT=$(ls "$PROJECT_DIR/.canvas/nodes/oscillator/"*.scm 2>/dev/null | wc -l)
if [ "$NODE_COUNT" -eq 2 ]; then
    echo "  OK: exactly 2 .scm files in oscillator canvas"
    PASS=$((PASS + 1))
else
    echo "  FAIL: expected 2 .scm files, got $NODE_COUNT"
    FAIL=$((FAIL + 1))
fi

# Git-readiness
echo ""
echo "--- Step 7: Git readiness ---"
(cd "$PROJECT_DIR" && git init -q && git add -A && git status --short) > /tmp/git-test.log 2>&1
assert "git init works" test -d "$PROJECT_DIR/.git"
if grep -q "\.env$" /tmp/git-test.log; then
    echo "  FAIL: .env is staged (should be gitignored)"
    FAIL=$((FAIL + 1))
else
    echo "  OK: .env not staged (gitignored)"
    PASS=$((PASS + 1))
fi
assert ".scm files staged" grep -q "controls.scm" /tmp/git-test.log

# --- Summary ---
echo ""
echo "==================================="
echo "  PASSED: $PASS"
echo "  FAILED: $FAIL"
echo "==================================="

[ "$FAIL" -eq 0 ] && exit 0 || exit 1
