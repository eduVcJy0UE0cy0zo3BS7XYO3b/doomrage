#!/bin/bash
set -e

cd "$(dirname "$0")"

echo "Building node crates..."
(cd node-crates && cargo build --target wasm32-wasip1 --release)

echo "Converting to WASM components..."
for name in add sub mul div sqrt abs clamp lerp; do
  wasm-tools component embed \
    nodes/math/${name}.wit \
    node-crates/target/wasm32-wasip1/release/node_${name}.wasm \
    --world node \
    -o /tmp/${name}_embedded.wasm
  wasm-tools component new \
    /tmp/${name}_embedded.wasm \
    -o nodes/math/${name}.wasm
  echo "  $name.wasm ($(stat -c%s nodes/math/${name}.wasm) bytes)"
done

echo "Done! All nodes built."
