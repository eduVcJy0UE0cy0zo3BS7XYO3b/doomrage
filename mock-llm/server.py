#!/usr/bin/env python3
"""
Mock LLM server for testing canvas-agent.
Returns scripted tool_use responses, then end_turn.

Usage:
  python server.py                     # default scenario: create oscillator
  python server.py scenario.json       # custom scenario from file

Add to litellm config.yaml:
  - model_name: mock-agent
    litellm_params:
      model: anthropic/mock-agent
      api_base: http://localhost:9999
      api_key: fake
"""

import json
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler

# Default scenario: create a canvas with two nodes
DEFAULT_SCENARIO = [
    # Step 1: create canvas
    {
        "role": "assistant",
        "stop_reason": "tool_use",
        "content": [
            {"type": "text", "text": "I'll create an oscillator with controls and a wave display."},
            {
                "type": "tool_use",
                "id": "tool_1",
                "name": "create_canvas",
                "input": {"name": "oscillator"}
            }
        ]
    },
    # Step 2: create controls node
    {
        "role": "assistant",
        "stop_reason": "tool_use",
        "content": [
            {
                "type": "tool_use",
                "id": "tool_2",
                "name": "create_node",
                "input": {
                    "canvas": "oscillator",
                    "label": "controls",
                    "code": '(define gain (widget "gain" \'slider 0.0 100.0))\n(define freq (widget "freq" \'slider 1.0 20.0))',
                    "exports": ["gain", "freq"]
                }
            }
        ]
    },
    # Step 3: create wave node
    {
        "role": "assistant",
        "stop_reason": "tool_use",
        "content": [
            {
                "type": "tool_use",
                "id": "tool_3",
                "name": "create_node",
                "input": {
                    "canvas": "oscillator",
                    "label": "wave",
                    "code": "(define pi 3.14159265)\n(define w 300.0)\n(define h 150.0)\n(define mid (/ h 2.0))\n(define n 60)\n\n(define (wave-points i acc)\n  (if (= i n) acc\n    (let* ((x (* (/ i n) w))\n           (y (+ mid (* gain (sin (* freq (/ i n) pi 4.0))))))\n      (wave-points (+ i 1) (cons (list x y) acc)))))\n\n(define pts (reverse (wave-points 0 '())))\n\n(canvas w h\n  (draw-rect 0 0 w h \"#f5f5f0\")\n  (draw-line 0 mid w mid \"#cccccc\" 1)\n  (draw-polyline pts \"#2266cc\" 2))",
                    "exports": [],
                    "imports": [["oscillator", "controls"]]
                }
            }
        ]
    },
    # Step 4: compute controls
    {
        "role": "assistant",
        "stop_reason": "tool_use",
        "content": [
            {
                "type": "tool_use",
                "id": "tool_4",
                "name": "compute_node",
                "input": {"canvas": "oscillator", "label": "controls"}
            }
        ]
    },
    # Step 5: compute wave
    {
        "role": "assistant",
        "stop_reason": "tool_use",
        "content": [
            {
                "type": "tool_use",
                "id": "tool_5",
                "name": "compute_node",
                "input": {"canvas": "oscillator", "label": "wave"}
            }
        ]
    },
    # Step 6: done
    {
        "role": "assistant",
        "stop_reason": "end_turn",
        "content": [
            {
                "type": "text",
                "text": "Done! Created oscillator with two nodes:\n- controls: gain and freq sliders\n- wave: canvas rendering of a sine wave\n\nConnect with the GUI to see the result."
            }
        ]
    }
]


class MockHandler(BaseHTTPRequestHandler):
    step = 0

    def do_POST(self):
        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length)
        request = json.loads(body) if body else {}

        model = request.get("model", "unknown")
        messages = request.get("messages", [])
        print(f"[mock] Request #{MockHandler.step + 1}: model={model}, messages={len(messages)}")

        if MockHandler.step < len(scenario):
            response = scenario[MockHandler.step]
            MockHandler.step += 1
        else:
            # Fallback: end turn
            response = {
                "stop_reason": "end_turn",
                "content": [{"type": "text", "text": "No more scripted responses."}]
            }

        # Wrap in Anthropic API format
        api_response = {
            "id": f"msg_mock_{MockHandler.step}",
            "type": "message",
            "role": "assistant",
            "model": model,
            "stop_reason": response["stop_reason"],
            "content": response["content"],
            "usage": {"input_tokens": 100, "output_tokens": 50}
        }

        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(api_response).encode())

    def log_message(self, format, *args):
        # Quieter logging
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 and sys.argv[1].isdigit() else 9999

    # Load custom scenario if provided
    scenario_file = None
    for arg in sys.argv[1:]:
        if arg.endswith(".json"):
            scenario_file = arg

    if scenario_file:
        with open(scenario_file) as f:
            scenario = json.load(f)
        print(f"[mock] Loaded scenario from {scenario_file} ({len(scenario)} steps)")
    else:
        scenario = DEFAULT_SCENARIO
        print(f"[mock] Using default scenario ({len(scenario)} steps)")

    print(f"[mock] Listening on http://localhost:{port}")
    server = HTTPServer(("127.0.0.1", port), MockHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n[mock] Stopped.")
