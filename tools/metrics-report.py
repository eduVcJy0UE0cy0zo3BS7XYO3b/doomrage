#!/usr/bin/env python3
"""Generate an HTML report with SVG charts from metrics.jsonl.

Usage:
    python3 tools/metrics-report.py ~/.canvas/metrics.jsonl > report.html
    # or
    python3 tools/metrics-report.py ~/.canvas/metrics.jsonl -o report.html && xdg-open report.html
"""

import json
import sys
import os

def read_jsonl(path):
    data = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    data.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return data

def svg_chart(title, series, width=800, height=200):
    """Generate SVG chart. series = [(label, color, values)]"""
    if not series or not series[0][2]:
        return f'<div class="chart"><h3>{title}</h3><p>No data</p></div>'

    all_vals = [v for _, _, vals in series for v in vals if v is not None]
    if not all_vals:
        return f'<div class="chart"><h3>{title}</h3><p>No data</p></div>'

    y_min = min(all_vals)
    y_max = max(all_vals)
    if y_min == y_max:
        y_max = y_min + 1

    pad_top, pad_bottom, pad_left, pad_right = 10, 30, 60, 20
    plot_w = width - pad_left - pad_right
    plot_h = height - pad_top - pad_bottom

    svg = f'<div class="chart"><h3>{title}</h3>\n'
    svg += f'<svg width="{width}" height="{height}" xmlns="http://www.w3.org/2000/svg">\n'

    # Background
    svg += f'<rect x="{pad_left}" y="{pad_top}" width="{plot_w}" height="{plot_h}" fill="#fafafa" stroke="#ddd"/>\n'

    # Y axis labels
    for i in range(5):
        y_val = y_min + (y_max - y_min) * (4 - i) / 4
        y_pos = pad_top + plot_h * i / 4
        label = f'{y_val:.1f}' if y_val != int(y_val) else str(int(y_val))
        svg += f'<text x="{pad_left - 5}" y="{y_pos + 4}" text-anchor="end" font-size="11" fill="#666">{label}</text>\n'
        svg += f'<line x1="{pad_left}" y1="{y_pos}" x2="{pad_left + plot_w}" y2="{y_pos}" stroke="#eee"/>\n'

    # Plot lines
    for label, color, vals in series:
        n = len(vals)
        if n < 2:
            continue
        points = []
        for i, v in enumerate(vals):
            if v is None:
                continue
            x = pad_left + plot_w * i / (n - 1)
            y = pad_top + plot_h * (1 - (v - y_min) / (y_max - y_min))
            points.append(f'{x:.1f},{y:.1f}')
        if points:
            svg += f'<polyline points="{" ".join(points)}" fill="none" stroke="{color}" stroke-width="2"/>\n'

    # Legend
    lx = pad_left + 10
    for i, (label, color, _) in enumerate(series):
        ly = pad_top + plot_h + 18
        svg += f'<rect x="{lx}" y="{ly - 8}" width="12" height="12" fill="{color}"/>\n'
        svg += f'<text x="{lx + 16}" y="{ly + 2}" font-size="11" fill="#333">{label}</text>\n'
        lx += len(label) * 7 + 30

    svg += '</svg></div>\n'
    return svg

def is_loadtest_format(data):
    return data and 't' in data[0] and 'phase' in data[0]

def generate_loadtest_report(data):
    if not data:
        return '<html><body><h1>No data</h1></body></html>'

    def extract(key):
        return [d.get(key) for d in data]

    last = data[-1]
    duration = last.get('t', 0)

    html = '''<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Load Test Report</title>
<style>
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; max-width: 900px; margin: 40px auto; padding: 0 20px; }
h1 { border-bottom: 2px solid #333; padding-bottom: 10px; }
h2 { margin-top: 30px; color: #555; }
.chart { margin: 20px 0; }
.chart h3 { margin: 5px 0; font-size: 14px; color: #444; }
.summary { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 15px; margin: 20px 0; }
.stat { background: #f5f5f5; padding: 15px; border-radius: 8px; }
.stat .value { font-size: 24px; font-weight: bold; }
.stat .label { font-size: 12px; color: #888; }
</style></head><body>
'''
    html += f'<h1>Load Test Report</h1>\n'
    html += f'<p>Duration: {duration:.0f}s | Samples: {len(data)}</p>\n'

    html += '<div class="summary">\n'
    for key, label in [
        ('clients', 'Clients'), ('ops_per_sec', 'ops/sec'), ('ops', 'Total ops'),
        ('cycles', 'Cycles'), ('errors', 'Errors'), ('error_rate', 'Error rate %'),
        ('p50_ms', 'p50 ms'), ('p99_ms', 'p99 ms'),
    ]:
        val = last.get(key, 0)
        val_str = f'{val:.1f}' if isinstance(val, float) else str(val)
        html += f'<div class="stat"><div class="value">{val_str}</div><div class="label">{label}</div></div>\n'
    html += '</div>\n'

    html += '<h2>Latency</h2>\n'
    html += svg_chart('p99 latency (ms)', [
        ('p99', '#cc2222', extract('p99_ms')),
        ('p50', '#2266cc', extract('p50_ms')),
    ])

    html += '<h2>Throughput</h2>\n'
    html += svg_chart('ops/sec', [('ops/sec', '#22aa66', extract('ops_per_sec'))])
    html += svg_chart('Concurrent clients', [('clients', '#448844', extract('clients'))])
    html += svg_chart('Total ops', [('ops', '#8844cc', extract('ops'))])
    html += svg_chart('Cycles completed', [('cycles', '#2288aa', extract('cycles'))])

    html += '<h2>Errors</h2>\n'
    html += svg_chart('Error rate (%)', [('error %', '#cc4444', extract('error_rate'))])
    html += svg_chart('Total errors', [('errors', '#cc2222', extract('errors'))])

    # Per-op breakdown table
    op_names = ['create_node', 'update_node', 'compute', 'node_state', 'defs', 'info', 'delete_node']
    op_labels = ['create-node', 'update-node', 'compute', 'node-state', 'defs', 'info', 'delete-node']
    html += '<h2>Per-operation latency</h2>\n'
    html += '<table style="width:100%;border-collapse:collapse;margin:20px 0">\n'
    html += '<tr><th style="padding:8px;border:1px solid #ddd;text-align:left">Operation</th>'
    html += '<th style="padding:8px;border:1px solid #ddd">Avg ms</th>'
    html += '<th style="padding:8px;border:1px solid #ddd">Count</th>'
    html += '<th style="padding:8px;border:1px solid #ddd">Errors</th></tr>\n'
    for op_key, op_label in zip(op_names, op_labels):
        avg = last.get(f'{op_key}__avg_ms', 0)
        count = last.get(f'{op_key}__count', 0)
        errors = last.get(f'{op_key}__errors', 0)
        color = '#cc2222' if errors else '#333'
        html += f'<tr><td style="padding:8px;border:1px solid #ddd"><code>{op_label}</code></td>'
        html += f'<td style="padding:8px;border:1px solid #ddd;text-align:right">{avg:.1f}</td>'
        html += f'<td style="padding:8px;border:1px solid #ddd;text-align:right">{count}</td>'
        html += f'<td style="padding:8px;border:1px solid #ddd;text-align:right;color:{color}">{errors}</td></tr>\n'
    html += '</table>\n'

    # Per-op latency chart
    colors = ['#2266cc', '#cc6622', '#22aa66', '#8844cc', '#aa4488', '#448888', '#cc2222']
    series = []
    for (op_key, op_label), color in zip(zip(op_names, op_labels), colors):
        series.append((op_label, color, extract(f'{op_key}__avg_ms')))
    html += svg_chart('Per-op avg latency (ms)', series)

    html += '</body></html>'
    return html

def generate_report(data):
    if not data:
        return '<html><body><h1>No metrics data</h1></body></html>'

    if is_loadtest_format(data):
        return generate_loadtest_report(data)

    t0 = data[0].get('ts', 0)
    times = [(d.get('ts', 0) - t0) for d in data]
    duration_sec = times[-1] if times else 0

    def extract(key):
        return [d.get(key) for d in data]

    html = '''<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>wasm-canvas metrics report</title>
<style>
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; max-width: 900px; margin: 40px auto; padding: 0 20px; background: #fff; color: #333; }
h1 { border-bottom: 2px solid #333; padding-bottom: 10px; }
h2 { margin-top: 30px; color: #555; }
.chart { margin: 20px 0; }
.chart h3 { margin: 5px 0; font-size: 14px; color: #444; }
.summary { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 15px; margin: 20px 0; }
.stat { background: #f5f5f5; padding: 15px; border-radius: 8px; }
.stat .value { font-size: 24px; font-weight: bold; }
.stat .label { font-size: 12px; color: #888; }
</style>
</head>
<body>
'''
    html += f'<h1>wasm-canvas metrics</h1>\n'
    html += f'<p>Duration: {duration_sec:.0f}s | Samples: {len(data)}</p>\n'

    # Summary cards
    last = data[-1]
    html += '<div class="summary">\n'
    for key, label in [
        ('compute_total', 'Computes'),
        ('compute_errors', 'Errors'),
        ('compute_duration_avg_ms', 'Avg compute ms'),
        ('nrepl_duration_count', 'nREPL requests'),
        ('nrepl_duration_avg_ms', 'Avg nREPL ms'),
        ('definitions_total', 'Definitions'),
        ('peers_connected', 'Peers'),
        ('def_requests', 'Def requests'),
    ]:
        val = last.get(key, 0)
        if val is None:
            val = 0
        if isinstance(val, float):
            val_str = f'{val:.2f}'
        else:
            val_str = str(val)
        html += f'<div class="stat"><div class="value">{val_str}</div><div class="label">{label}</div></div>\n'
    html += '</div>\n'

    # Charts
    html += '<h2>Compute</h2>\n'
    html += svg_chart('Compute total', [
        ('computes', '#2266cc', extract('compute_total')),
        ('errors', '#cc2222', extract('compute_errors')),
    ])
    html += svg_chart('Avg compute duration (ms)', [
        ('avg ms', '#22aa66', extract('compute_duration_avg_ms')),
    ])
    html += svg_chart('Pending computes', [
        ('pending', '#ff8800', extract('pending_computes')),
    ])

    html += '<h2>nREPL</h2>\n'
    html += svg_chart('nREPL requests', [
        ('total', '#8844cc', extract('nrepl_duration_count')),
    ])
    html += svg_chart('Avg nREPL latency (ms)', [
        ('avg ms', '#cc4488', extract('nrepl_duration_avg_ms')),
    ])

    html += '<h2>Network</h2>\n'
    html += svg_chart('Peers connected', [
        ('peers', '#2288aa', extract('peers_connected')),
    ])
    html += svg_chart('Definition sharing', [
        ('requests', '#aa6622', extract('def_requests')),
        ('served', '#22aa22', extract('def_responses_served')),
        ('received', '#2222aa', extract('def_responses_received')),
    ])
    html += svg_chart('Network values received', [
        ('values', '#666', extract('network_values_received')),
    ])

    html += '<h2>Data</h2>\n'
    html += svg_chart('Definitions in Name DB', [
        ('definitions', '#448844', extract('definitions_total')),
    ])

    html += '</body></html>'
    return html

if __name__ == '__main__':
    if len(sys.argv) < 2:
        print(f'Usage: {sys.argv[0]} <metrics.jsonl> [-o output.html]', file=sys.stderr)
        sys.exit(1)

    data = read_jsonl(sys.argv[1])

    output = None
    if '-o' in sys.argv:
        idx = sys.argv.index('-o')
        if idx + 1 < len(sys.argv):
            output = sys.argv[idx + 1]

    html = generate_report(data)

    if output:
        with open(output, 'w') as f:
            f.write(html)
        print(f'Report written to {output}', file=sys.stderr)
    else:
        print(html)
