.PHONY: run peer repl agent mock test-e2e build nodes clean metrics-report

run: nodes
	cargo run

peer:
	cargo run -p wasm-canvas-peer

repl:
	cargo run -p nrepl --bin client

agent:
	cargo run -p canvas-agent --

mock:
	python3 mock-llm/server.py

test-e2e:
	./tests/run-in-container.sh

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

metrics-report:
	python3 tools/metrics-report.py ~/.canvas/metrics.jsonl -o /tmp/metrics-report.html
	@echo "Report: /tmp/metrics-report.html"
	@xdg-open /tmp/metrics-report.html 2>/dev/null || open /tmp/metrics-report.html 2>/dev/null || echo "Open /tmp/metrics-report.html in browser"

clean:
	cargo clean
	cd node-crates && cargo clean
