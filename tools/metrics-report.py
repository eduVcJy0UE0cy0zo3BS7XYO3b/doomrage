#!/usr/bin/env python3
"""Generate HTML reports from loadtest metrics.

Single report:
    python3 tools/metrics-report.py loadtest-output/20260326-224430/loadtest-results.jsonl -o report.html

Compare two runs:
    python3 tools/metrics-report.py --compare loadtest-output/A loadtest-output/B -o compare.html

Trend over selected runs (by tag / date range):
    python3 tools/metrics-report.py --trend loadtest-output/ --tag baseline --tag fast -o trend.html
    python3 tools/metrics-report.py --trend loadtest-output/ --since 20260326-210000 --until 20260326-220000 -o trend.html

Tag a run:
    python3 tools/metrics-report.py --set-tags loadtest-output/20260326-224430 baseline v2

Index (regenerate loadtest-output/index.html):
    python3 tools/metrics-report.py --index loadtest-output/
"""

import json
import sys
import os
import argparse
from datetime import datetime


# ---------------------------------------------------------------------------
# I/O helpers
# ---------------------------------------------------------------------------


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


def read_meta(run_dir):
    """Read meta.json from run directory. Returns dict with at least 'tags': []."""
    path = os.path.join(run_dir, "meta.json")
    if os.path.exists(path):
        try:
            with open(path) as f:
                return json.load(f)
        except Exception:
            pass
    return {"tags": []}


def write_meta(run_dir, meta):
    path = os.path.join(run_dir, "meta.json")
    with open(path, "w") as f:
        json.dump(meta, f, indent=2)
    print(f"Saved {path}", file=sys.stderr)


def find_runs(output_dir):
    """Return list of (run_id, run_dir) sorted oldest→newest."""
    runs = []
    for name in sorted(os.listdir(output_dir)):
        d = os.path.join(output_dir, name)
        if os.path.isdir(d) and os.path.exists(
            os.path.join(d, "loadtest-results.jsonl")
        ):
            runs.append((name, d))
    return runs


def run_summary(run_dir, data=None):
    """Return dict with summary scalars for a run."""
    if data is None:
        jf = os.path.join(run_dir, "loadtest-results.jsonl")
        data = read_jsonl(jf) if os.path.exists(jf) else []
    if not data:
        return {}
    last = data[-1]
    first = data[0]
    # best ops/sec across all samples
    best_ops = max((d.get("ops_per_sec") or 0) for d in data)
    best_clients = None
    for d in data:
        if (d.get("ops_per_sec") or 0) == best_ops:
            best_clients = d.get("clients")
            break
    return {
        "duration_s": last.get("t", 0),
        "samples": len(data),
        "max_clients": last.get("clients", 0),
        "best_ops_sec": best_ops,
        "best_clients": best_clients,
        "final_p50_ms": last.get("p50_ms"),
        "final_p99_ms": last.get("p99_ms"),
        "total_ops": last.get("ops", 0),
        "total_cycles": last.get("cycles", 0),
        "errors": last.get("errors", 0),
        "error_rate": last.get("error_rate", 0),
    }


def common_metric_keys(datasets):
    """Return sorted list of numeric metric keys present in ALL datasets."""
    if not datasets:
        return []
    # collect keys per dataset (keys present in at least one sample and numeric)
    key_sets = []
    for data in datasets:
        ks = set()
        for d in data:
            for k, v in d.items():
                if isinstance(v, (int, float)) and v is not None:
                    ks.add(k)
        key_sets.append(ks)
    common = key_sets[0]
    for ks in key_sets[1:]:
        common = common & ks
    # exclude bookkeeping fields
    exclude = {"t"}
    return sorted(common - exclude)


# ---------------------------------------------------------------------------
# SVG chart primitive
# ---------------------------------------------------------------------------


def _fmt_time(secs):
    """Format seconds as mm:ss or hh:mm:ss."""
    secs = int(secs)
    h, rem = divmod(secs, 3600)
    m, s = divmod(rem, 60)
    if h:
        return f"{h}:{m:02d}:{s:02d}"
    return f"{m}:{s:02d}"


def svg_chart(
    title,
    series,
    width=800,
    height=200,
    hlines=None,
    vlines=None,
    right_series=None,
    x_times=None,  # list of float seconds, same length as series values — draws X axis time labels
):
    """
    series       = [(label, color, values)]
    hlines       = [(y_value, color, label)]  horizontal annotation lines (left axis)
    vlines       = [(x_index, color, label)]  vertical annotation lines
    right_series = [(label, color, values)]   plotted on right axis (dashed)
    x_times      = [float, ...]               time in seconds for X axis labels (mm:ss)
    """
    if not series or not series[0][2]:
        return f'<div class="chart"><h3>{title}</h3><p>No data</p></div>\n'

    all_vals = [v for _, _, vals in series for v in vals if v is not None]
    if not all_vals:
        return f'<div class="chart"><h3>{title}</h3><p>No data</p></div>\n'

    y_min = min(all_vals)
    y_max = max(all_vals)
    if y_min == y_max:
        y_max = y_min + 1

    pad_top = 10
    pad_bottom = 30 if not x_times else 44  # extra space for time labels
    pad_left = 60
    pad_right = 50 if right_series else 20
    plot_w = width - pad_left - pad_right
    plot_h = height - pad_top - pad_bottom

    n_pts = max(len(vals) for _, _, vals in series)

    def to_x(i, n=None):
        nn = n if n is not None else n_pts
        return pad_left + plot_w * i / max(nn - 1, 1)

    def to_y(v, vmin, vmax):
        return pad_top + plot_h * (1 - (v - vmin) / (vmax - vmin))

    svg = f'<div class="chart"><h3>{title}</h3>\n'
    svg += (
        f'<svg width="{width}" height="{height}" xmlns="http://www.w3.org/2000/svg">\n'
    )

    # Background
    svg += f'<rect x="{pad_left}" y="{pad_top}" width="{plot_w}" height="{plot_h}" fill="#fafafa" stroke="#ddd"/>\n'

    # Y axis labels (left)
    for i in range(5):
        y_val = y_min + (y_max - y_min) * (4 - i) / 4
        y_pos = pad_top + plot_h * i / 4
        lbl = f"{y_val:.1f}" if y_val != int(y_val) else str(int(y_val))
        svg += f'<text x="{pad_left - 5}" y="{y_pos + 4}" text-anchor="end" font-size="11" fill="#666">{lbl}</text>\n'
        svg += f'<line x1="{pad_left}" y1="{y_pos}" x2="{pad_left + plot_w}" y2="{y_pos}" stroke="#eee"/>\n'

    # X axis time labels
    if x_times and len(x_times) >= 2:
        t_min = x_times[0]
        t_max = x_times[-1]
        t_span = max(t_max - t_min, 1)
        # choose ~6 tick positions
        n_ticks = 6
        tick_interval = t_span / n_ticks
        # round to nice number
        for nice in [1, 2, 5, 10, 15, 20, 30, 60, 120, 300, 600]:
            if tick_interval <= nice:
                tick_interval = nice
                break
        tick_t = t_min
        y_tick = pad_top + plot_h
        while tick_t <= t_max + tick_interval * 0.1:
            x_frac = (tick_t - t_min) / t_span
            tx = pad_left + plot_w * x_frac
            svg += f'<line x1="{tx:.1f}" y1="{y_tick}" x2="{tx:.1f}" y2="{y_tick + 4}" stroke="#aaa" stroke-width="1"/>\n'
            svg += f'<text x="{tx:.1f}" y="{y_tick + 14}" text-anchor="middle" font-size="10" fill="#888">{_fmt_time(tick_t)}</text>\n'
            tick_t += tick_interval
        # X axis label "time (mm:ss)"
        svg += f'<text x="{pad_left + plot_w / 2:.1f}" y="{pad_top + plot_h + 30}" text-anchor="middle" font-size="10" fill="#aaa">time (mm:ss)</text>\n'

    # Horizontal annotation lines
    if hlines:
        for h_val, h_color, h_label in hlines:
            if y_min <= h_val <= y_max * 1.05:
                hy = to_y(max(h_val, y_min), y_min, y_max)
                svg += f'<line x1="{pad_left}" y1="{hy:.1f}" x2="{pad_left + plot_w}" y2="{hy:.1f}" stroke="{h_color}" stroke-width="1" stroke-dasharray="4,3"/>\n'
                svg += f'<text x="{pad_left + plot_w - 2}" y="{hy - 3}" text-anchor="end" font-size="10" fill="{h_color}">{h_label}</text>\n'

    # Vertical annotation lines
    if vlines:
        for vx_i, v_color, v_label in vlines:
            vx = to_x(vx_i)
            svg += f'<line x1="{vx:.1f}" y1="{pad_top}" x2="{vx:.1f}" y2="{pad_top + plot_h}" stroke="{v_color}" stroke-width="1" stroke-dasharray="3,3"/>\n'
            if v_label:
                svg += f'<text x="{vx + 3:.1f}" y="{pad_top + 11}" font-size="10" fill="{v_color}">{v_label}</text>\n'

    # Right axis
    if right_series:
        r_all = [v for _, _, vals in right_series for v in vals if v is not None]
        if r_all:
            r_min, r_max = min(r_all), max(r_all)
            if r_min == r_max:
                r_max = r_min + 1
            for i in range(5):
                r_val = r_min + (r_max - r_min) * (4 - i) / 4
                y_pos = pad_top + plot_h * i / 4
                svg += f'<text x="{pad_left + plot_w + 4}" y="{y_pos + 4}" text-anchor="start" font-size="11" fill="#888">{int(round(r_val))}</text>\n'
            for lbl, color, vals in right_series:
                n = len(vals)
                if n < 2:
                    continue
                pts = []
                for i, v in enumerate(vals):
                    if v is None:
                        continue
                    pts.append(f"{to_x(i, n):.1f},{to_y(v, r_min, r_max):.1f}")
                if pts:
                    svg += f'<polyline points="{" ".join(pts)}" fill="none" stroke="{color}" stroke-width="1.5" stroke-dasharray="5,3"/>\n'

    # Main series (left axis)
    for lbl, color, vals in series:
        n = len(vals)
        if n < 2:
            continue
        pts = []
        for i, v in enumerate(vals):
            if v is None:
                continue
            pts.append(f"{to_x(i, n):.1f},{to_y(v, y_min, y_max):.1f}")
        if pts:
            svg += f'<polyline points="{" ".join(pts)}" fill="none" stroke="{color}" stroke-width="2"/>\n'

    # Legend
    all_legend = list(series)
    if right_series:
        all_legend += right_series
    if hlines:
        all_legend += [(lbl, col, []) for _, col, lbl in hlines]
    lx = pad_left + 10
    for lbl, color, _ in all_legend:
        ly = pad_top + plot_h + 18
        svg += f'<rect x="{lx}" y="{ly - 8}" width="12" height="12" fill="{color}"/>\n'
        svg += f'<text x="{lx + 16}" y="{ly + 2}" font-size="11" fill="#333">{lbl}</text>\n'
        lx += len(lbl) * 7 + 30

    svg += "</svg></div>\n"
    return svg


def svg_scatter(title, points, width=800, height=200):
    """
    Scatter plot for trend view.
    points = [(x_label, y_value, color, dot_label)]
    """
    if not points:
        return f'<div class="chart"><h3>{title}</h3><p>No data</p></div>\n'

    ys = [p[1] for p in points if p[1] is not None]
    if not ys:
        return f'<div class="chart"><h3>{title}</h3><p>No data</p></div>\n'

    y_min, y_max = min(ys), max(ys)
    if y_min == y_max:
        y_max = y_min + 1

    pad_top, pad_bottom, pad_left, pad_right = 10, 50, 60, 20
    plot_w = width - pad_left - pad_right
    plot_h = height - pad_top - pad_bottom
    n = len(points)

    svg = f'<div class="chart"><h3>{title}</h3>\n'
    svg += (
        f'<svg width="{width}" height="{height}" xmlns="http://www.w3.org/2000/svg">\n'
    )
    svg += f'<rect x="{pad_left}" y="{pad_top}" width="{plot_w}" height="{plot_h}" fill="#fafafa" stroke="#ddd"/>\n'

    # Y axis
    for i in range(5):
        y_val = y_min + (y_max - y_min) * (4 - i) / 4
        y_pos = pad_top + plot_h * i / 4
        lbl = f"{y_val:.1f}" if y_val != int(y_val) else str(int(y_val))
        svg += f'<text x="{pad_left - 5}" y="{y_pos + 4}" text-anchor="end" font-size="11" fill="#666">{lbl}</text>\n'
        svg += f'<line x1="{pad_left}" y1="{y_pos}" x2="{pad_left + plot_w}" y2="{y_pos}" stroke="#eee"/>\n'

    # Connect dots with line
    line_pts = []
    for i, (x_lbl, y_val, color, dot_lbl) in enumerate(points):
        if y_val is None:
            continue
        x = pad_left + plot_w * i / max(n - 1, 1)
        y = pad_top + plot_h * (1 - (y_val - y_min) / (y_max - y_min))
        line_pts.append(f"{x:.1f},{y:.1f}")
    if len(line_pts) >= 2:
        svg += f'<polyline points="{" ".join(line_pts)}" fill="none" stroke="#aaa" stroke-width="1" stroke-dasharray="3,3"/>\n'

    # Dots + labels
    for i, (x_lbl, y_val, color, dot_lbl) in enumerate(points):
        if y_val is None:
            continue
        x = pad_left + plot_w * i / max(n - 1, 1)
        y = pad_top + plot_h * (1 - (y_val - y_min) / (y_max - y_min))
        svg += f'<circle cx="{x:.1f}" cy="{y:.1f}" r="5" fill="{color}" stroke="#fff" stroke-width="1"/>\n'
        # X label (run id / date)
        svg += f'<text x="{x:.1f}" y="{pad_top + plot_h + 14}" text-anchor="middle" font-size="9" fill="#666">{x_lbl}</text>\n'
        # dot value label
        svg += f'<text x="{x:.1f}" y="{y - 8:.1f}" text-anchor="middle" font-size="9" fill="{color}">{dot_lbl}</text>\n'

    svg += "</svg></div>\n"
    return svg


# ---------------------------------------------------------------------------
# CSS / page shell
# ---------------------------------------------------------------------------

PAGE_CSS = """
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
       max-width: 1100px; margin: 40px auto; padding: 0 20px; }
h1 { border-bottom: 2px solid #333; padding-bottom: 10px; }
h2 { margin-top: 30px; color: #555; }
h3 { margin-top: 20px; color: #666; font-size: 15px; }
.chart { margin: 20px 0; }
.chart h3 { margin: 5px 0; font-size: 14px; color: #444; }
.summary { display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
           gap: 12px; margin: 20px 0; }
.stat { background: #f5f5f5; padding: 12px; border-radius: 8px; }
.stat .value { font-size: 22px; font-weight: bold; }
.stat .label { font-size: 11px; color: #888; }
.tag { display: inline-block; background: #2266cc; color: #fff;
       border-radius: 4px; padding: 2px 8px; font-size: 12px; margin: 2px; }
.badge-warn { background: #ff6600; }
.badge-ok   { background: #22aa66; }
table { width: 100%; border-collapse: collapse; margin: 16px 0; }
th, td { padding: 8px; border: 1px solid #ddd; text-align: left; }
th { background: #f0f0f0; }
tr:hover { background: #fafafa; }
.run-col { vertical-align: top; width: 50%; padding: 0 10px; }
.compare-grid { display: flex; gap: 20px; }
.diff-better { color: #22aa66; font-weight: bold; }
.diff-worse  { color: #cc2222; font-weight: bold; }
.diff-same   { color: #888; }
.note { background: #fffbe6; border-left: 4px solid #f5c518;
        padding: 10px 14px; margin: 14px 0; font-size: 13px; }
"""


def page(title, body):
    return f"""<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>{title}</title>
<style>{PAGE_CSS}</style></head><body>
{body}
</body></html>"""


# ---------------------------------------------------------------------------
# Single run report
# ---------------------------------------------------------------------------


def client_change_vlines(data):
    clients_vals = [d.get("clients") for d in data]
    vlines = []
    prev_c = None
    for i, c in enumerate(clients_vals):
        if c is not None and c != prev_c and prev_c is not None:
            vlines.append((i, "#888888", f"{c} clients"))
        if c is not None:
            prev_c = c
    return vlines, clients_vals


def load_host_metrics(run_dir):
    """Load host-metrics.jsonl if present. Returns [] if missing."""
    path = os.path.join(run_dir, "host-metrics.jsonl")
    if not os.path.exists(path):
        return []
    return read_jsonl(path)


def fmt_bytes(b):
    """Format bytes as human-readable string."""
    if b is None:
        return "—"
    for unit, thresh in [("GB", 1024**3), ("MB", 1024**2), ("KB", 1024), ("B", 1)]:
        if b >= thresh:
            return f"{b / thresh:.1f} {unit}"
    return f"{b} B"


def generate_host_section(hdata, loadtest_t0_ts=None):
    """Generate HTML section for host metrics.
    hdata = list of host-metrics.jsonl rows.
    loadtest_t0_ts = unix timestamp of loadtest start (to align X axis).
    """
    if not hdata:
        return ""

    # Align time to loadtest start if possible
    if loadtest_t0_ts:
        t0 = loadtest_t0_ts
    else:
        t0 = hdata[0].get("ts", 0)

    times = [d.get("ts", 0) - t0 for d in hdata]

    def extract(key):
        return [d.get(key) for d in hdata]

    def mb(vals):
        return [v / 1024 / 1024 if v is not None else None for v in vals]

    last = hdata[-1]
    body = "<h2>Host &amp; Container Metrics</h2>\n"

    # Summary cards
    cpu_vals = [v for v in extract("host_cpu_pct") if v is not None]
    mem_used = last.get("host_mem_used_bytes", 0) or 0
    mem_total = last.get("host_mem_total_bytes", 1) or 1
    peer_cpu_vals = [v for v in extract("peer_cpu_pct") if v is not None]
    peer_mem = last.get("peer_mem_used_bytes", 0) or 0
    peer_limit = last.get("peer_mem_limit_bytes", 0) or 0

    body += '<div class="summary">\n'
    stats = [
        ("Avg host CPU", f"{sum(cpu_vals) / len(cpu_vals):.1f}%" if cpu_vals else "—"),
        ("Peak host CPU", f"{max(cpu_vals):.1f}%" if cpu_vals else "—"),
        ("Host mem used", fmt_bytes(mem_used)),
        ("Host mem total", fmt_bytes(mem_total)),
        (
            "Avg peer CPU",
            f"{sum(peer_cpu_vals) / len(peer_cpu_vals):.1f}%" if peer_cpu_vals else "—",
        ),
        ("Peak peer CPU", f"{max(peer_cpu_vals):.1f}%" if peer_cpu_vals else "—"),
        ("Peer mem used", fmt_bytes(peer_mem)),
        ("Peer mem limit", fmt_bytes(peer_limit) if peer_limit else "—"),
    ]
    for label, val in stats:
        body += f'<div class="stat"><div class="value" style="font-size:16px">{val}</div><div class="label">{label}</div></div>\n'
    body += "</div>\n"

    kw = dict(x_times=times, height=180)

    # CPU
    body += svg_chart(
        "CPU usage (%)",
        [
            ("host CPU", "#cc6622", extract("host_cpu_pct")),
            ("peer CPU", "#2266cc", extract("peer_cpu_pct")),
        ],
        **kw,
    )

    # Memory MB
    body += svg_chart(
        "Memory usage (MB)",
        [
            ("host mem", "#8844cc", mb(extract("host_mem_used_bytes"))),
            ("peer mem", "#2288aa", mb(extract("peer_mem_used_bytes"))),
        ],
        **kw,
    )

    # Disk throughput MB/s
    disk_rd = [
        v / 1024 / 1024 if v is not None else None for v in extract("disk_read_bytes_s")
    ]
    disk_wr = [
        v / 1024 / 1024 if v is not None else None
        for v in extract("disk_write_bytes_s")
    ]
    if any(v for v in disk_rd + disk_wr if v):
        body += svg_chart(
            "Disk throughput (MB/s)",
            [
                ("read", "#22aa66", disk_rd),
                ("write", "#cc2222", disk_wr),
            ],
            **kw,
        )
        body += svg_chart(
            "Disk IOPS",
            [
                ("read IOPS", "#22aa66", extract("disk_read_iops")),
                ("write IOPS", "#cc2222", extract("disk_write_iops")),
            ],
            **kw,
        )

    # Network KB/s (host)
    net_rx = [v / 1024 if v is not None else None for v in extract("net_rx_bytes_s")]
    net_tx = [v / 1024 if v is not None else None for v in extract("net_tx_bytes_s")]
    body += svg_chart(
        "Host network throughput (KB/s)",
        [
            ("rx", "#2266cc", net_rx),
            ("tx", "#cc6622", net_tx),
        ],
        **kw,
    )

    # Container block IO (cumulative → delta already done by podman stats)
    peer_blk_rd = mb(extract("peer_blk_read_bytes"))
    peer_blk_wr = mb(extract("peer_blk_write_bytes"))
    if any(v for v in peer_blk_rd + peer_blk_wr if v):
        body += svg_chart(
            "Peer container block I/O (MB cumulative)",
            [
                ("blk read", "#22aa66", peer_blk_rd),
                ("blk write", "#cc2222", peer_blk_wr),
            ],
            **kw,
        )

    return body


def generate_single_report(data, run_id="", meta=None, run_dir=None):
    if not data:
        return page("No data", "<h1>No data</h1>")

    meta = meta or {}
    tags = meta.get("tags", [])
    last = data[-1]
    duration = last.get("t", 0)

    vlines, clients_vals = client_change_vlines(data)
    x_times = [d.get("t", 0) for d in data]

    def extract(key):
        return [d.get(key) for d in data]

    body = "<h1>Load Test Report</h1>\n"
    if run_id:
        body += f"<p><strong>Run:</strong> {run_id}</p>\n"
    if tags:
        body += (
            "<p>" + "".join(f'<span class="tag">{t}</span>' for t in tags) + "</p>\n"
        )
    body += f"<p>Duration: {duration:.0f}s | Samples: {len(data)}</p>\n"

    # Summary cards
    body += '<div class="summary">\n'
    smry = run_summary("", data)
    for key, label in [
        ("max_clients", "Max clients"),
        ("best_ops_sec", "Best ops/sec"),
        ("best_clients", "Best at N clients"),
        ("final_p50_ms", "p50 ms (final)"),
        ("final_p99_ms", "p99 ms (final)"),
        ("total_ops", "Total ops"),
        ("total_cycles", "Cycles"),
        ("errors", "Errors"),
    ]:
        val = smry.get(key, 0) or 0
        val_str = f"{val:.1f}" if isinstance(val, float) else str(val)
        body += f'<div class="stat"><div class="value">{val_str}</div><div class="label">{label}</div></div>\n'
    body += "</div>\n"

    kw = dict(x_times=x_times, vlines=vlines)

    body += "<h2>Latency</h2>\n"
    body += svg_chart(
        "p99 / p50 latency (ms)  |  clients (right axis, dashed)",
        [("p99", "#cc2222", extract("p99_ms")), ("p50", "#2266cc", extract("p50_ms"))],
        hlines=[(1000, "#ff6600", "hard limit 1000ms")],
        right_series=[("clients", "#44aa44", clients_vals)],
        **kw,
    )

    body += "<h2>Throughput</h2>\n"
    if any(d.get("ops_per_sec") is not None for d in data):
        body += svg_chart(
            "ops/sec  |  clients (right axis, dashed)",
            [("ops/sec", "#22aa66", extract("ops_per_sec"))],
            right_series=[("clients", "#44aa44", clients_vals)],
            **kw,
        )
    body += svg_chart(
        "Concurrent clients", [("clients", "#448844", clients_vals)], x_times=x_times
    )
    body += svg_chart(
        "Total ops", [("ops", "#8844cc", extract("ops"))], x_times=x_times
    )
    body += svg_chart(
        "Cycles completed", [("cycles", "#2288aa", extract("cycles"))], x_times=x_times
    )

    body += "<h2>Errors</h2>\n"
    body += svg_chart(
        "Error rate (%)",
        [("error %", "#cc4444", extract("error_rate"))],
        x_times=x_times,
    )

    # Per-op
    op_names = [
        "create_node",
        "update_node",
        "compute",
        "node_state",
        "defs",
        "info",
        "delete_node",
    ]
    op_labels = [
        "create-node",
        "update-node",
        "compute",
        "node-state",
        "defs",
        "info",
        "delete-node",
    ]
    op_keys_present = [k for k in op_names if last.get(f"{k}__avg_ms") is not None]
    if op_keys_present:
        body += "<h2>Per-operation latency</h2>\n"
        body += "<table>\n<tr><th>Operation</th><th>Avg ms</th><th>Count</th><th>Errors</th></tr>\n"
        for op_key, op_label in zip(op_names, op_labels):
            if op_key not in op_keys_present:
                continue
            avg = last.get(f"{op_key}__avg_ms", 0)
            count = last.get(f"{op_key}__count", 0)
            errors = last.get(f"{op_key}__errors", 0)
            color = "#cc2222" if errors else "#333"
            body += f"<tr><td><code>{op_label}</code></td><td>{avg:.1f}</td><td>{count}</td>"
            body += f'<td style="color:{color}">{errors}</td></tr>\n'
        body += "</table>\n"
        colors = [
            "#2266cc",
            "#cc6622",
            "#22aa66",
            "#8844cc",
            "#aa4488",
            "#448888",
            "#cc2222",
        ]
        series = []
        for (op_key, op_label), color in zip(zip(op_names, op_labels), colors):
            if op_key in op_keys_present:
                series.append((op_label, color, extract(f"{op_key}__avg_ms")))
        body += svg_chart("Per-op avg latency (ms)", series, x_times=x_times)

    # Peer-side metrics (DB, memory, compute from peer's perspective)
    if run_dir:
        peer_path = os.path.join(run_dir, "peer-metrics.jsonl")
        if os.path.exists(peer_path):
            pdata = read_jsonl(peer_path)
            if pdata:
                pt0 = pdata[0].get("ts", 0)
                ptimes = [d.get("ts", 0) - pt0 for d in pdata]
                plast = pdata[-1]

                def pextract(key):
                    return [d.get(key) for d in pdata]

                def pmb(key):
                    return [d.get(key, 0) / 1024 / 1024 if d.get(key) else None for d in pdata]

                body += "<h2>Database (SurrealDB)</h2>\n"
                body += '<div class="summary">\n'
                db_queries = plast.get("db_queries", plast.get("db_query_count", 0))
                db_avg = plast.get("db_query_avg_ms", 0)
                db_errors = plast.get("db_errors", 0)
                for val, label in [
                    (str(db_queries), "DB queries"),
                    (f"{db_avg:.1f} ms" if db_avg else "—", "Avg query"),
                    (str(db_errors), "DB errors"),
                ]:
                    body += f'<div class="stat"><div class="value">{val}</div><div class="label">{label}</div></div>\n'
                body += "</div>\n"
                body += svg_chart("DB queries (cumulative)", [("queries", "#448844", pextract("db_queries") or pextract("db_query_count"))], x_times=ptimes)
                if any(d.get("db_query_avg_ms") for d in pdata):
                    body += svg_chart("Avg DB query (ms)", [("avg ms", "#aa6622", pextract("db_query_avg_ms"))], x_times=ptimes)

                body += "<h2>Memory</h2>\n"
                rss_mb = plast.get("memory_rss_bytes", 0) / 1024 / 1024
                alloc_mb = plast.get("memory_allocated_bytes", 0) / 1024 / 1024
                resident_mb = plast.get("memory_resident_bytes", 0) / 1024 / 1024
                body += '<div class="summary">\n'
                for val, label in [
                    (f"{rss_mb:.0f} MB", "RSS"),
                    (f"{alloc_mb:.0f} MB", "jemalloc allocated"),
                    (f"{resident_mb:.0f} MB", "jemalloc resident"),
                    (str(plast.get("env_cache_size", 0)), "Env cache"),
                ]:
                    body += f'<div class="stat"><div class="value">{val}</div><div class="label">{label}</div></div>\n'
                body += "</div>\n"
                body += svg_chart("RSS (MB)", [("RSS", "#cc2222", pmb("memory_rss_bytes"))], x_times=ptimes)
                if any(d.get("memory_allocated_bytes") for d in pdata):
                    body += svg_chart("jemalloc (MB)", [
                        ("allocated", "#2266cc", pmb("memory_allocated_bytes")),
                        ("resident", "#22aa66", pmb("memory_resident_bytes")),
                    ], x_times=ptimes)
                body += svg_chart("Env cache size", [("cached envs", "#8844cc", pextract("env_cache_size"))], x_times=ptimes)

                body += "<h2>Compute (peer-side)</h2>\n"
                body += svg_chart("Compute total", [("computes", "#2266cc", pextract("compute_total"))], x_times=ptimes)
                if any(d.get("compute_duration_avg_ms") for d in pdata):
                    body += svg_chart("Avg compute (ms)", [("avg ms", "#22aa66", pextract("compute_duration_avg_ms"))], x_times=ptimes)
                body += svg_chart("Pending computes", [("pending", "#ff8800", pextract("pending_computes"))], x_times=ptimes)
                body += svg_chart("Definitions in Name DB", [("defs", "#448844", pextract("definitions_total"))], x_times=ptimes)

    # Host metrics
    if run_dir:
        hdata = load_host_metrics(run_dir)
        if hdata:
            body += generate_host_section(hdata)

    return page(f"Load Test — {run_id}", body)


# ---------------------------------------------------------------------------
# Compare two runs
# ---------------------------------------------------------------------------

COMPARE_COLORS = ["#2266cc", "#cc6622"]

# Metrics shown in compare (label, key, higher_is_better, format)
COMPARE_METRICS = [
    ("Best ops/sec", "best_ops_sec", True, "{:.1f}"),
    ("Best at clients", "best_clients", True, "{}"),
    ("p50 ms (final)", "final_p50_ms", False, "{:.0f}"),
    ("p99 ms (final)", "final_p99_ms", False, "{:.0f}"),
    ("Total cycles", "total_cycles", True, "{}"),
    ("Error rate %", "error_rate", False, "{:.2f}"),
    ("Duration s", "duration_s", None, "{:.0f}"),
]

# Time-series keys available in both old and new format
TIMESERIES_KEYS = [
    ("p99_ms", "p99 latency (ms)", "#cc2222"),
    ("p50_ms", "p50 latency (ms)", "#2266cc"),
    ("ops_per_sec", "ops/sec", "#22aa66"),
    ("error_rate", "error rate %", "#cc4444"),
    ("create_node__avg_ms", "create-node avg ms", "#2266cc"),
    ("update_node__avg_ms", "update-node avg ms", "#cc6622"),
    ("compute__avg_ms", "compute avg ms", "#22aa66"),
    ("node_state__avg_ms", "node-state avg ms", "#8844cc"),
    ("delete_node__avg_ms", "delete-node avg ms", "#cc2222"),
]


def generate_compare_report(run_a_dir, run_b_dir):
    id_a = os.path.basename(run_a_dir.rstrip("/"))
    id_b = os.path.basename(run_b_dir.rstrip("/"))
    meta_a = read_meta(run_a_dir)
    meta_b = read_meta(run_b_dir)

    data_a = read_jsonl(os.path.join(run_a_dir, "loadtest-results.jsonl"))
    data_b = read_jsonl(os.path.join(run_b_dir, "loadtest-results.jsonl"))

    smry_a = run_summary(run_a_dir, data_a)
    smry_b = run_summary(run_b_dir, data_b)

    # Intersection of numeric metric keys
    common_keys = common_metric_keys([data_a, data_b])

    body = "<h1>Compare Runs</h1>\n"

    # Run headers
    body += '<div class="compare-grid">\n'
    for run_id, meta in [(id_a, meta_a), (id_b, meta_b)]:
        tags = meta.get("tags", [])
        tag_html = "".join(f'<span class="tag">{t}</span>' for t in tags)
        body += f'<div class="run-col"><strong>{run_id}</strong> {tag_html}</div>\n'
    body += "</div>\n"

    body += '<div class="note">Only metrics present in <strong>both</strong> runs are compared.</div>\n'

    # Summary comparison table
    body += "<h2>Summary</h2>\n"
    body += (
        "<table>\n<tr><th>Metric</th><th>"
        + id_a
        + "</th><th>"
        + id_b
        + "</th><th>Diff</th></tr>\n"
    )
    for label, key, higher_better, fmt in COMPARE_METRICS:
        va = smry_a.get(key)
        vb = smry_b.get(key)
        if va is None and vb is None:
            continue
        sa = fmt.format(va) if va is not None else "—"
        sb = fmt.format(vb) if vb is not None else "—"
        diff_html = ""
        if va is not None and vb is not None and higher_better is not None:
            delta = vb - va
            pct = (delta / va * 100) if va != 0 else 0
            sign = "+" if delta >= 0 else ""
            better = (delta > 0) == higher_better
            cls = (
                "diff-better"
                if better
                else ("diff-worse" if delta != 0 else "diff-same")
            )
            diff_html = f'<span class="{cls}">{sign}{pct:.1f}%</span>'
        body += (
            f"<tr><td>{label}</td><td>{sa}</td><td>{sb}</td><td>{diff_html}</td></tr>\n"
        )
    body += "</table>\n"

    # Time-series overlay charts (only common keys)
    body += "<h2>Time-series overlay</h2>\n"
    body += f'<p style="font-size:13px;color:#666">Solid = {id_a} &nbsp; Dashed = {id_b}</p>\n'

    shown = 0
    for ts_key, ts_label, base_color in TIMESERIES_KEYS:
        if ts_key not in common_keys:
            continue

        def extract_norm(data, key):
            """Extract values; normalise x axis to [0..1] by sample index."""
            return [d.get(key) for d in data]

        vals_a = extract_norm(data_a, ts_key)
        vals_b = extract_norm(data_b, ts_key)

        # Pad shorter series with None so charts align by relative progress
        # (both are time-indexed separately, we just overlay them)
        vlines_a, clients_a = client_change_vlines(data_a)

        # Put both on same chart as separate series; Series B is rendered dashed
        # via a trick: we use right_series for B with matching scale
        # But right_series has independent scale. Instead we normalise both to
        # the same y scale by computing combined min/max.
        all_v = [v for v in vals_a + vals_b if v is not None]
        if not all_v:
            continue

        hlines = (
            [(1000, "#ff6600", "hard limit 1000ms")] if ts_key == "p99_ms" else None
        )
        body += svg_chart(
            ts_label,
            [(id_a, base_color, vals_a)],
            hlines=hlines,
            right_series=[(id_b, "#888888", vals_b)],
            vlines=vlines_a,
        )
        shown += 1

    if shown == 0:
        body += "<p>No common time-series metrics found.</p>\n"

    # Missing metrics note
    keys_a = common_metric_keys([data_a])
    keys_b = common_metric_keys([data_b])
    only_a = sorted(
        set(keys_a) - set(keys_b) - {"t", "clients", "phase", "ops", "cycles", "errors"}
    )
    only_b = sorted(
        set(keys_b) - set(keys_a) - {"t", "clients", "phase", "ops", "cycles", "errors"}
    )
    if only_a or only_b:
        body += "<h2>Metrics not in both runs</h2>\n"
        if only_a:
            body += f"<p>Only in <strong>{id_a}</strong>: <code>{', '.join(only_a)}</code></p>\n"
        if only_b:
            body += f"<p>Only in <strong>{id_b}</strong>: <code>{', '.join(only_b)}</code></p>\n"

    return page(f"Compare {id_a} vs {id_b}", body)


# ---------------------------------------------------------------------------
# Trend report
# ---------------------------------------------------------------------------

TREND_DOT_COLORS = [
    "#2266cc",
    "#cc6622",
    "#22aa66",
    "#8844cc",
    "#aa4488",
    "#448888",
    "#cc2222",
    "#66aa22",
    "#2288cc",
    "#cc22aa",
]

TREND_SCALAR_METRICS = [
    ("best_ops_sec", "Best ops/sec", True),
    ("best_clients", "Best clients", True),
    ("final_p99_ms", "p99 ms (final)", False),
    ("final_p50_ms", "p50 ms (final)", False),
    ("total_cycles", "Total cycles", True),
    ("error_rate", "Error rate %", False),
]


def generate_trend_report(runs, title="Trend"):
    """
    runs = [(run_id, run_dir, meta, data), ...]  sorted oldest→newest
    """
    if not runs:
        return page("No runs", "<h1>No runs selected</h1>")

    all_data = [data for _, _, _, data in runs]
    common_keys = common_metric_keys(all_data)

    body = f"<h1>{title}</h1>\n"
    body += f"<p>{len(runs)} runs analysed</p>\n"

    # Runs table
    body += "<h2>Selected Runs</h2>\n"
    body += "<table>\n<tr><th>#</th><th>Run ID</th><th>Tags</th><th>Best ops/sec</th>"
    body += "<th>Best clients</th><th>p99 ms</th><th>Errors</th></tr>\n"
    for idx, (run_id, run_dir, meta, data) in enumerate(runs):
        smry = run_summary(run_dir, data)
        tags = meta.get("tags", [])
        tag_html = "".join(f'<span class="tag">{t}</span>' for t in tags) or "—"
        color = TREND_DOT_COLORS[idx % len(TREND_DOT_COLORS)]
        body += f'<tr><td><span style="color:{color}">&#11044;</span> {idx + 1}</td>'
        body += f"<td>{run_id}</td><td>{tag_html}</td>"
        body += f"<td>{smry.get('best_ops_sec', 0):.1f}</td>"
        body += f"<td>{smry.get('best_clients') or '—'}</td>"
        body += f"<td>{smry.get('final_p99_ms') or '—'}</td>"
        body += f"<td>{smry.get('errors', 0)}</td></tr>\n"
    body += "</table>\n"

    body += '<div class="note">Only metrics present in <strong>all</strong> selected runs are shown in charts.</div>\n'

    # Scalar trend scatter charts
    body += "<h2>Scalar trends over time</h2>\n"
    for smry_key, label, higher_better in TREND_SCALAR_METRICS:
        points = []
        for idx, (run_id, run_dir, meta, data) in enumerate(runs):
            smry = run_summary(run_dir, data)
            val = smry.get(smry_key)
            if val is None:
                continue
            color = TREND_DOT_COLORS[idx % len(TREND_DOT_COLORS)]
            short_id = run_id[-11:] if len(run_id) > 11 else run_id  # MMDD-HHMMSS
            dot_lbl = f"{val:.1f}" if isinstance(val, float) else str(val)
            points.append((short_id, val, color, dot_lbl))
        if len(points) >= 2:
            body += svg_scatter(label, points)

    # Time-series overlay for common keys
    ts_keys_to_show = [(k, l, c) for k, l, c in TIMESERIES_KEYS if k in common_keys]
    if ts_keys_to_show:
        body += "<h2>Time-series overlay (common metrics)</h2>\n"
        body += "<p style='font-size:13px;color:#666'>Each run is a separate line. X axis = sample index (relative progress).</p>\n"
        for ts_key, ts_label, base_color in ts_keys_to_show:
            series = []
            for idx, (run_id, run_dir, meta, data) in enumerate(runs):
                vals = [d.get(ts_key) for d in data]
                color = TREND_DOT_COLORS[idx % len(TREND_DOT_COLORS)]
                short_id = run_id[-11:] if len(run_id) > 11 else run_id
                series.append((short_id, color, vals))
            vlines_first, _ = client_change_vlines(runs[0][3])
            body += svg_chart(ts_label, series, vlines=vlines_first)
    else:
        body += "<h2>Time-series overlay</h2>\n<p>No common time-series metrics across all selected runs.</p>\n"

    return page(title, body)


# ---------------------------------------------------------------------------
# Index page
# ---------------------------------------------------------------------------


def generate_index(output_dir, runs=None):
    """Generate index.html with filtering UI."""
    if runs is None:
        runs = find_runs(output_dir)

    # Collect all tags
    all_tags = set()
    run_info = []
    for run_id, run_dir in runs:
        meta = read_meta(run_dir)
        data = read_jsonl(os.path.join(run_dir, "loadtest-results.jsonl"))
        smry = run_summary(run_dir, data)
        tags = meta.get("tags", [])
        all_tags.update(tags)
        run_info.append((run_id, run_dir, meta, smry, tags))

    all_tags = sorted(all_tags)

    # Parse run_id as datetime for display
    def run_dt(run_id):
        try:
            return datetime.strptime(run_id, "%Y%m%d-%H%M%S")
        except Exception:
            return None

    html = f"""<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Load Test History</title>
<style>
{PAGE_CSS}
#controls {{ margin: 20px 0; padding: 16px; background: #f8f8f8; border-radius: 8px; }}
#controls label {{ margin-right: 12px; font-size: 13px; }}
#controls input[type=text], #controls input[type=date] {{
    border: 1px solid #ccc; border-radius: 4px; padding: 4px 8px; font-size: 13px; }}
.btn {{ background: #2266cc; color: #fff; border: none; border-radius: 4px;
        padding: 6px 14px; font-size: 13px; cursor: pointer; margin-left: 8px; }}
.btn:hover {{ background: #1a4fa0; }}
.btn-sec {{ background: #888; }}
.btn-sec:hover {{ background: #555; }}
.run-row {{ cursor: pointer; }}
.run-row.selected {{ background: #e8f0ff; }}
.run-row.hidden {{ display: none; }}
#selection-bar {{ margin: 12px 0; padding: 10px 14px; background: #fffbe6;
                  border-left: 4px solid #f5c518; display: none; font-size: 13px; }}
</style>
</head><body>
<h1>Load Test History</h1>
<div id="controls">
  <label>Tags: <input type="text" id="filter-tags" placeholder="e.g. baseline fast" oninput="applyFilters()"/></label>
  <label>Since: <input type="date" id="filter-since" onchange="applyFilters()"/></label>
  <label>Until: <input type="date" id="filter-until" onchange="applyFilters()"/></label>
  <button class="btn btn-sec" onclick="clearFilters()">Clear</button>
</div>
<div id="selection-bar">
  <span id="sel-count">0</span> run(s) selected &nbsp;
  <button class="btn" onclick="openCompare()">Compare 2</button>
  <button class="btn" onclick="openTrend()">Trend</button>
  <button class="btn btn-sec" onclick="clearSelection()">Clear selection</button>
</div>
<table id="runs-table">
<tr>
  <th></th><th>Date / Time</th><th>Tags</th>
  <th>Best ops/sec</th><th>Best clients</th>
  <th>p99 ms</th><th>Errors</th><th>Duration</th><th>Report</th>
</tr>
"""
    for run_id, run_dir, meta, smry, tags in reversed(run_info):
        dt = run_dt(run_id)
        dt_str = dt.strftime("%Y-%m-%d %H:%M:%S") if dt else run_id
        dt_iso = dt.strftime("%Y-%m-%d") if dt else ""
        tag_html = "".join(f'<span class="tag">{t}</span>' for t in tags)
        report_link = (
            f'<a href="{run_id}/report.html">view</a>'
            if os.path.exists(os.path.join(run_dir, "report.html"))
            else "—"
        )
        p99 = smry.get("final_p99_ms")
        p99_str = f"{p99:.0f}" if p99 is not None else "—"
        p99_cls = ' style="color:#cc2222;font-weight:bold"' if (p99 or 0) > 1000 else ""
        ops = smry.get("best_ops_sec", 0)
        clients = smry.get("best_clients") or "—"
        errs = smry.get("errors", 0)
        dur = smry.get("duration_s", 0)
        tags_data = " ".join(tags)
        html += f"""<tr class="run-row" data-run="{run_id}" data-date="{dt_iso}" data-tags="{tags_data}" onclick="toggleRow(this)">
  <td><input type="checkbox" class="row-check" onclick="event.stopPropagation();toggleRow(this.closest('tr'))"></td>
  <td>{dt_str}</td>
  <td>{tag_html}</td>
  <td>{ops:.1f}</td><td>{clients}</td>
  <td{p99_cls}>{p99_str}</td>
  <td>{errs}</td>
  <td>{dur:.0f}s</td>
  <td>{report_link}</td>
</tr>\n"""

    html += r"""</table>
<script>
function applyFilters() {
    var tagFilter = document.getElementById('filter-tags').value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    var since = document.getElementById('filter-since').value;
    var until = document.getElementById('filter-until').value;
    document.querySelectorAll('.run-row').forEach(function(row) {
        var rowTags = row.dataset.tags.toLowerCase();
        var rowDate = row.dataset.date;
        var tagOk = tagFilter.length === 0 || tagFilter.every(function(t){ return rowTags.indexOf(t) >= 0; });
        var sinceOk = !since || rowDate >= since;
        var untilOk = !until || rowDate <= until;
        row.classList.toggle('hidden', !(tagOk && sinceOk && untilOk));
    });
    updateSelectionBar();
}
function clearFilters() {
    document.getElementById('filter-tags').value = '';
    document.getElementById('filter-since').value = '';
    document.getElementById('filter-until').value = '';
    applyFilters();
}
function toggleRow(row) {
    row.classList.toggle('selected');
    var cb = row.querySelector('.row-check');
    if (cb) cb.checked = row.classList.contains('selected');
    updateSelectionBar();
}
function updateSelectionBar() {
    var sel = getSelected();
    var bar = document.getElementById('selection-bar');
    document.getElementById('sel-count').textContent = sel.length;
    bar.style.display = sel.length > 0 ? 'block' : 'none';
}
function getSelected() {
    return Array.from(document.querySelectorAll('.run-row.selected')).map(function(r){ return r.dataset.run; });
}
function clearSelection() {
    document.querySelectorAll('.run-row.selected').forEach(function(r){
        r.classList.remove('selected');
        var cb = r.querySelector('.row-check'); if (cb) cb.checked = false;
    });
    updateSelectionBar();
}
function openCompare() {
    var sel = getSelected();
    if (sel.length !== 2) { alert('Select exactly 2 runs to compare'); return; }
    window.location.href = 'compare.html?a=' + encodeURIComponent(sel[0]) + '&b=' + encodeURIComponent(sel[1]);
}
function openTrend() {
    var sel = getSelected();
    if (sel.length < 2) { alert('Select at least 2 runs for trend'); return; }
    window.location.href = 'trend.html?runs=' + sel.map(encodeURIComponent).join(',');
}
</script>
</body></html>"""

    return html


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def cmd_set_tags(run_dir, tags):
    meta = read_meta(run_dir)
    meta["tags"] = tags
    write_meta(run_dir, meta)
    print(f"Tags set: {tags}")


def cmd_add_tags(run_dir, tags):
    meta = read_meta(run_dir)
    existing = set(meta.get("tags", []))
    existing.update(tags)
    meta["tags"] = sorted(existing)
    write_meta(run_dir, meta)
    print(f"Tags now: {meta['tags']}")


def cmd_index(output_dir, out_path=None):
    runs = find_runs(output_dir)
    html = generate_index(output_dir, runs)
    out = out_path or os.path.join(output_dir, "index.html")
    with open(out, "w") as f:
        f.write(html)
    print(f"Index written to {out}", file=sys.stderr)


def cmd_compare(run_a_dir, run_b_dir, out_path):
    html = generate_compare_report(run_a_dir, run_b_dir)
    with open(out_path, "w") as f:
        f.write(html)
    print(f"Compare report: {out_path}", file=sys.stderr)


def cmd_trend(
    output_dir, tags=None, since=None, until=None, run_ids=None, out_path=None
):
    all_runs = find_runs(output_dir)
    selected = []
    for run_id, run_dir in all_runs:
        if run_ids and run_id not in run_ids:
            continue
        dt = None
        try:
            dt = datetime.strptime(run_id, "%Y%m%d-%H%M%S")
        except Exception:
            pass
        if since and dt and dt < datetime.strptime(since, "%Y%m%d-%H%M%S"):
            continue
        if until and dt and dt > datetime.strptime(until, "%Y%m%d-%H%M%S"):
            continue
        meta = read_meta(run_dir)
        run_tags = set(meta.get("tags", []))
        if tags and not set(tags).issubset(run_tags):
            continue
        data = read_jsonl(os.path.join(run_dir, "loadtest-results.jsonl"))
        selected.append((run_id, run_dir, meta, data))

    tag_str = ", ".join(tags) if tags else "all"
    title = f"Trend — {tag_str}"
    html = generate_trend_report(selected, title=title)
    out = out_path or os.path.join(output_dir, "trend.html")
    with open(out, "w") as f:
        f.write(html)
    print(f"Trend report: {out}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description="loadtest metrics report tool")
    sub = parser.add_subparsers(dest="cmd")

    # Single report (default / positional)
    p_single = sub.add_parser("report", help="Single run report")
    p_single.add_argument("jsonl", help="loadtest-results.jsonl path")
    p_single.add_argument("-o", "--output", help="output HTML path")

    # Compare
    p_cmp = sub.add_parser("compare", help="Compare two runs")
    p_cmp.add_argument("run_a", help="First run directory")
    p_cmp.add_argument("run_b", help="Second run directory")
    p_cmp.add_argument("-o", "--output", help="output HTML path")

    # Trend
    p_trend = sub.add_parser("trend", help="Trend across runs")
    p_trend.add_argument("output_dir", help="loadtest-output directory")
    p_trend.add_argument(
        "--tag", action="append", dest="tags", help="Filter by tag (AND)"
    )
    p_trend.add_argument("--since", help="Start datetime YYYYMMDD-HHMMSS")
    p_trend.add_argument("--until", help="End datetime YYYYMMDD-HHMMSS")
    p_trend.add_argument("--runs", help="Comma-separated run IDs")
    p_trend.add_argument("-o", "--output", help="output HTML path")

    # Index
    p_idx = sub.add_parser("index", help="Regenerate index.html")
    p_idx.add_argument("output_dir", help="loadtest-output directory")
    p_idx.add_argument("-o", "--output", help="output HTML path")

    # Tag
    p_tag = sub.add_parser("tag", help="Set tags on a run")
    p_tag.add_argument("run_dir", help="Run directory")
    p_tag.add_argument("tags", nargs="+", help="Tags to set")
    p_tag.add_argument("--add", action="store_true", help="Add (not replace) tags")

    # Legacy single-file mode: python3 metrics-report.py file.jsonl [-o out.html]
    # Handle before argparse so paths aren't mistaken for subcommands.
    SUBCMDS = {"report", "compare", "trend", "index", "tag"}
    if (
        len(sys.argv) >= 2
        and sys.argv[1] not in SUBCMDS
        and not sys.argv[1].startswith("-")
    ):
        jsonl_path = sys.argv[1]
        out = None
        if "-o" in sys.argv:
            idx = sys.argv.index("-o")
            out = sys.argv[idx + 1]
        if os.path.isdir(jsonl_path):
            jsonl_path = os.path.join(jsonl_path, "loadtest-results.jsonl")
        data = read_jsonl(jsonl_path)
        run_dir = os.path.dirname(os.path.abspath(jsonl_path))
        run_id = os.path.basename(run_dir)
        meta = read_meta(run_dir)
        if data and "t" in data[0] and "phase" in data[0]:
            html = generate_single_report(data, run_id, meta, run_dir=run_dir)
        else:
            html = generate_peer_report(data)
        if out:
            with open(out, "w") as f:
                f.write(html)
            print(f"Report written to {out}", file=sys.stderr)
        else:
            print(html)
        return

    args = parser.parse_args()

    if args.cmd is None:
        parser.print_help()
        return

    if args.cmd == "report":
        path = args.jsonl
        if os.path.isdir(path):
            path = os.path.join(path, "loadtest-results.jsonl")
        data = read_jsonl(path)
        run_dir = os.path.dirname(path)
        run_id = os.path.basename(run_dir)
        meta = read_meta(run_dir)
        html = generate_single_report(data, run_id, meta, run_dir=run_dir)
        out = args.output or os.path.join(run_dir, "report.html")
        with open(out, "w") as f:
            f.write(html)
        print(f"Report: {out}", file=sys.stderr)

    elif args.cmd == "compare":
        out = args.output
        if not out:
            base = os.path.dirname(args.run_a.rstrip("/"))
            out = os.path.join(base, "compare.html")
        cmd_compare(args.run_a, args.run_b, out)

    elif args.cmd == "trend":
        run_ids = args.runs.split(",") if args.runs else None
        cmd_trend(
            args.output_dir,
            tags=args.tags,
            since=args.since,
            until=args.until,
            run_ids=run_ids,
            out_path=args.output,
        )

    elif args.cmd == "index":
        cmd_index(args.output_dir, args.output)

    elif args.cmd == "tag":
        if args.add:
            cmd_add_tags(args.run_dir, args.tags)
        else:
            cmd_set_tags(args.run_dir, args.tags)


# ---------------------------------------------------------------------------
# Peer metrics report (unchanged from original, kept for compat)
# ---------------------------------------------------------------------------


def generate_peer_report(data):
    if not data:
        return page("No data", "<h1>No metrics data</h1>")

    t0 = data[0].get("ts", 0)
    times = [(d.get("ts", 0) - t0) for d in data]
    duration_sec = times[-1] if times else 0

    def extract(key):
        return [d.get(key) for d in data]

    last = data[-1]

    body = "<h1>wasm-canvas metrics</h1>\n"
    body += f"<p>Duration: {duration_sec:.0f}s | Samples: {len(data)}</p>\n"

    body += '<div class="summary">\n'
    for key, label in [
        ("compute_total", "Computes"),
        ("compute_errors", "Errors"),
        ("compute_duration_avg_ms", "Avg compute ms"),
        ("nrepl_duration_count", "nREPL requests"),
        ("nrepl_duration_avg_ms", "Avg nREPL ms"),
        ("definitions_total", "Definitions"),
        ("peers_connected", "Peers"),
        ("def_requests", "Def requests"),
    ]:
        val = last.get(key) or 0
        val_str = f"{val:.2f}" if isinstance(val, float) else str(val)
        body += f'<div class="stat"><div class="value">{val_str}</div><div class="label">{label}</div></div>\n'
    body += "</div>\n"

    body += "<h2>Compute</h2>\n"
    body += svg_chart(
        "Compute total",
        [
            ("computes", "#2266cc", extract("compute_total")),
            ("errors", "#cc2222", extract("compute_errors")),
        ],
    )
    body += svg_chart(
        "Avg compute duration (ms)",
        [("avg ms", "#22aa66", extract("compute_duration_avg_ms"))],
    )
    body += svg_chart(
        "Pending computes", [("pending", "#ff8800", extract("pending_computes"))]
    )

    body += "<h2>nREPL</h2>\n"
    body += svg_chart(
        "nREPL requests", [("total", "#8844cc", extract("nrepl_duration_count"))]
    )
    body += svg_chart(
        "Avg nREPL latency (ms)",
        [("avg ms", "#cc4488", extract("nrepl_duration_avg_ms"))],
    )

    body += "<h2>Network</h2>\n"
    body += svg_chart(
        "Peers connected", [("peers", "#2288aa", extract("peers_connected"))]
    )
    body += svg_chart(
        "Definition sharing",
        [
            ("requests", "#aa6622", extract("def_requests")),
            ("served", "#22aa22", extract("def_responses_served")),
            ("received", "#2222aa", extract("def_responses_received")),
        ],
    )
    body += svg_chart(
        "Network values received",
        [("values", "#666", extract("network_values_received"))],
    )

    body += "<h2>Data</h2>\n"
    body += svg_chart(
        "Definitions in Name DB",
        [("definitions", "#448844", extract("definitions_total"))],
    )

    return page("wasm-canvas metrics", body)


if __name__ == "__main__":
    main()
