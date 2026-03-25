.PHONY: run peer repl build nodes clean

run: nodes
	cargo run

peer:
	cargo run -p wasm-canvas-peer

repl:
	cargo run -p nrepl --bin client

build: nodes
	cargo build --release

nodes:
	@echo "Building WASM nodes..."
	@cd node-crates && cargo build --target wasm32-wasip1 --release 2>&1 | tail -1
	@for name in add sub mul div sqrt abs clamp lerp; do \
		wasm-tools component embed \
			nodes/math/$$name.wit \
			node-crates/target/wasm32-wasip1/release/node_$$name.wasm \
			--world node \
			-o /tmp/$$name_embedded.wasm && \
		wasm-tools component new \
			/tmp/$$name_embedded.wasm \
			-o nodes/math/$$name.wasm; \
	done
	@echo "All nodes built."

clean:
	cargo clean
	cd node-crates && cargo clean
