#!/usr/bin/env python3
"""AG2 Performance Analyst Agent.

Reads loadtest results and peer metrics, analyzes through LLM,
writes timestamped report.

Usage:
    source .venv/bin/activate
    python agents/perf-analyst.py                          # latest run
    python agents/perf-analyst.py loadtest-output/20260327-103507  # specific run
    python agents/perf-analyst.py --model gemini-flash     # use cheap model
"""

import json
import os
import sys
import glob
from datetime import datetime
from autogen import ConversableAgent

# --- Config ---

LITELLM_BASE = os.environ.get("LITELLM_BASE", "http://localhost:4000")
LITELLM_KEY = os.environ.get("LITELLM_KEY", "sk-litellm-master-key-local")
DEFAULT_MODEL = os.environ.get("PERF_MODEL", "gemini-flash")  # cheap by default

# --- Data loading ---

def find_latest_run():
    runs = sorted(glob.glob("loadtest-output/2*"))
    if not runs:
        print("No loadtest runs found. Run: ./tests/loadtest/run.sh --soak-minutes 0")
        sys.exit(1)
    return runs[-1]

def load_jsonl_last(path):
    if not os.path.exists(path):
        return None
    lines = open(path).readlines()
    if not lines:
        return None
    return json.loads(lines[-1])

def load_jsonl_first(path):
    if not os.path.exists(path):
        return None
    lines = open(path).readlines()
    if not lines:
        return None
    return json.loads(lines[0])

def build_context(run_dir):
    """Build a text context with all loadtest data for the LLM."""
    ctx = f"# Loadtest run: {os.path.basename(run_dir)}\n\n"

    # HW info
    hw_path = os.path.join(run_dir, "hw-info.json")
    if os.path.exists(hw_path):
        hw = json.load(open(hw_path))
        ctx += "## Hardware\n"
        for k, v in sorted(hw.items()):
            ctx += f"- {k}: {v}\n"
        ctx += "\n"

    # Per-op latency
    lt = load_jsonl_last(os.path.join(run_dir, "loadtest-results.jsonl"))
    if lt:
        ctx += "## Per-op latency (last sample)\n"
        ops = [(k.replace('__avg_ms', ''), lt.get(k, 0), lt.get(k.replace('avg_ms', 'count'), 0))
               for k in sorted(lt) if k.endswith('__avg_ms')]
        total_time = sum(m * c for _, m, c in ops)
        for name, ms, count in sorted(ops, key=lambda x: -x[1]):
            pct = ms * count / total_time * 100 if total_time > 0 else 0
            ctx += f"- {name}: {ms:.1f} ms avg ({count:.0f} calls, {pct:.1f}% of cycle)\n"
        ctx += f"\n- ops/sec: {lt.get('ops_per_sec', 0):.1f}\n"
        ctx += f"- p50: {lt.get('p50_ms', 0):.0f} ms\n"
        ctx += f"- p99: {lt.get('p99_ms', 0):.0f} ms\n"
        ctx += f"- errors: {lt.get('errors', 0)}\n"
        ctx += f"- clients: {lt.get('clients', 0)}\n\n"

    # Peer metrics
    peer_first = load_jsonl_first(os.path.join(run_dir, "peer-metrics.jsonl"))
    peer_last = load_jsonl_last(os.path.join(run_dir, "peer-metrics.jsonl"))
    if peer_first and peer_last:
        ctx += "## Peer metrics (server-side)\n"
        dt = peer_last['ts'] - peer_first['ts']
        ctx += f"- Duration: {dt:.0f}s\n"
        ctx += f"- RSS: {peer_first['memory_rss_bytes']//1024//1024} → {peer_last['memory_rss_bytes']//1024//1024} MB\n"
        ctx += f"- jemalloc allocated: {peer_first['memory_allocated_bytes']//1024//1024} → {peer_last['memory_allocated_bytes']//1024//1024} MB\n"
        ctx += f"- Env cache: {peer_first['env_cache_size']} → {peer_last['env_cache_size']}\n"
        db_delta = peer_last['db_queries'] - peer_first['db_queries']
        ctx += f"- DB queries: {db_delta} (avg {peer_last.get('db_query_avg_ms', 0):.1f} ms)\n"
        ctx += f"- DB errors: {peer_last['db_errors']}\n"
        computes = peer_last['compute_total'] - peer_first['compute_total']
        ctx += f"- Computes: {computes}\n"
        if computes > 0:
            ctx += f"- DB queries per compute: {db_delta / computes:.0f}\n"
        ctx += f"- Compute avg: {peer_last.get('compute_duration_avg_ms', 0):.0f} ms\n"
        ctx += "\n"

    return ctx

# --- Main ---

def main():
    # Parse args
    run_dir = None
    model = DEFAULT_MODEL
    for arg in sys.argv[1:]:
        if arg.startswith("--model="):
            model = arg.split("=", 1)[1]
        elif arg.startswith("--model"):
            pass  # next arg is model
        elif sys.argv[sys.argv.index(arg) - 1] == "--model" if arg != sys.argv[0] else False:
            model = arg
        elif os.path.isdir(arg):
            run_dir = arg

    if not run_dir:
        run_dir = find_latest_run()

    print(f"Run:   {run_dir}")
    print(f"Model: {model} (via litellm @ {LITELLM_BASE})")
    print()

    # Build context
    context = build_context(run_dir)
    if len(context) < 100:
        print("Not enough data to analyze.")
        sys.exit(1)

    # Create AG2 agent
    llm_config = {
        "config_list": [{
            "model": model,
            "base_url": f"{LITELLM_BASE}/v1",
            "api_key": LITELLM_KEY,
        }],
        "temperature": 0.1,
    }

    analyst = ConversableAgent(
        name="perf_analyst",
        system_message="""Ты performance-аналитик. Тебе дают данные нагрузочного тестирования.
Проанализируй и напиши краткий отчёт:

1. **Железо и окружение** — одна строка
2. **Throughput** — ops/sec, при скольких клиентах
3. **Top-3 bottleneck** — какие операции самые медленные, % от цикла
4. **DB нагрузка** — queries per compute, avg latency
5. **Memory** — RSS trend, есть ли утечка, env cache
6. **Рекомендации** — что оптимизировать, приоритет

Будь конкретным. Используй числа из данных. Не придумывай.
Пиши на русском.""",
        llm_config=llm_config,
        human_input_mode="NEVER",
    )

    user = ConversableAgent(
        name="user",
        human_input_mode="NEVER",
        max_consecutive_auto_reply=0,
    )

    print("--- Analyzing... ---")
    print()

    result = user.initiate_chat(
        analyst,
        message=f"Проанализируй результаты нагрузочного тестирования:\n\n{context}",
        max_turns=1,
    )

    # Extract response
    response = result.chat_history[-1]["content"] if result.chat_history else "No response"

    # Save report
    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    report_dir = f"reports/perf/{timestamp}"
    os.makedirs(report_dir, exist_ok=True)
    report_path = os.path.join(report_dir, "analysis.md")

    with open(report_path, "w") as f:
        f.write(f"# Performance Analysis\n\n")
        f.write(f"**Run:** {os.path.basename(run_dir)}\n")
        f.write(f"**Model:** {model}\n")
        f.write(f"**Date:** {datetime.now().isoformat()}\n\n---\n\n")
        f.write(response)
        f.write(f"\n\n---\n*Generated by AG2 perf-analyst agent using {model}*\n")

    print()
    print(f"Report: {report_path}")
    print(f"HTML:   xdg-open {run_dir}/report.html")

if __name__ == "__main__":
    main()
