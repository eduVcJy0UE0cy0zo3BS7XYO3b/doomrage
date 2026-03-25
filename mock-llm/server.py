#!/usr/bin/env python3
"""
Mock LLM server for testing canvas-agent via litellm.
Responds in OpenAI chat completions format.
litellm config: model: openai/mock-agent, api_base: http://...:9999/v1

Usage:
  python server.py [port]
"""

import json
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler

# Default scenario: create oscillator with controls + wave
DEFAULT_SCENARIO = [
    # Step 1: create canvas
    {
        "tool_calls": [
            {"id": "call_1", "type": "function", "function": {
                "name": "create_canvas",
                "arguments": json.dumps({"name": "oscillator"})
            }}
        ]
    },
    # Step 2: create controls node
    {
        "tool_calls": [
            {"id": "call_2", "type": "function", "function": {
                "name": "create_node",
                "arguments": json.dumps({
                    "canvas": "oscillator",
                    "label": "controls",
                    "code": "(define gain (widget \"gain\" 'slider 0.0 100.0))\n(define freq (widget \"freq\" 'slider 1.0 20.0))",
                    "exports": ["gain", "freq"]
                })
            }}
        ]
    },
    # Step 3: create wave node
    {
        "tool_calls": [
            {"id": "call_3", "type": "function", "function": {
                "name": "create_node",
                "arguments": json.dumps({
                    "canvas": "oscillator",
                    "label": "wave",
                    "code": "(define pi 3.14159265)\n(define w 300.0)\n(define h 150.0)\n(define mid (/ h 2.0))\n(define n 60)\n\n(define (wave-points i acc)\n  (if (= i n) acc\n    (let* ((x (* (/ i n) w))\n           (y (+ mid (* gain (sin (* freq (/ i n) pi 4.0))))))\n      (wave-points (+ i 1) (cons (list x y) acc)))))\n\n(define pts (reverse (wave-points 0 '())))\n\n(canvas w h\n  (draw-rect 0 0 w h \"#f5f5f0\")\n  (draw-line 0 mid w mid \"#cccccc\" 1)\n  (draw-polyline pts \"#2266cc\" 2))",
                    "exports": [],
                    "imports": [["oscillator", "controls"]]
                })
            }}
        ]
    },
    # Step 4: compute controls
    {
        "tool_calls": [
            {"id": "call_4", "type": "function", "function": {
                "name": "compute_node",
                "arguments": json.dumps({"canvas": "oscillator", "label": "controls"})
            }}
        ]
    },
    # Step 5: compute wave
    {
        "tool_calls": [
            {"id": "call_5", "type": "function", "function": {
                "name": "compute_node",
                "arguments": json.dumps({"canvas": "oscillator", "label": "wave"})
            }}
        ]
    },
    # Step 6: done
    {
        "content": "Done! Created oscillator with two nodes:\n- controls: gain and freq sliders\n- wave: canvas rendering of a sine wave\n\nConnect with the GUI to see the result."
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
        print(f"[mock] Request #{MockHandler.step + 1}: model={model}, {len(messages)} messages")

        if MockHandler.step < len(scenario):
            step_data = scenario[MockHandler.step]
            MockHandler.step += 1
        else:
            step_data = {"content": "No more scripted responses."}

        # Build OpenAI chat completion response
        message = {"role": "assistant"}
        finish_reason = "stop"

        if "tool_calls" in step_data:
            message["tool_calls"] = step_data["tool_calls"]
            message["content"] = step_data.get("content")
            finish_reason = "tool_calls"
        else:
            message["content"] = step_data.get("content", "")

        response = {
            "id": f"chatcmpl-mock-{MockHandler.step}",
            "object": "chat.completion",
            "model": model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason,
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
        }

        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(response).encode())

    def log_message(self, format, *args):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 and sys.argv[1].isdigit() else 9999

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

    print(f"[mock] Listening on http://0.0.0.0:{port} (OpenAI format)")
    server = HTTPServer(("0.0.0.0", port), MockHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n[mock] Stopped.")
