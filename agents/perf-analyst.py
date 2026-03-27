#!/usr/bin/env python3
"""AG2 Multi-Agent Performance Pipeline.

Three agents work together:
  1. LoadTest Runner — runs tests, collects data
  2. Perf Analyst — reads data + source code, finds bottlenecks
  3. Report Writer — compiles final report

Agents have tools: read files, run shell commands, write reports.

Usage:
    source .venv/bin/activate
    python agents/perf-analyst.py                          # full pipeline
    python agents/perf-analyst.py --skip-test              # analyze existing data
    python agents/perf-analyst.py --model=claude-haiku     # use specific model
"""

import json
import os
import sys
import glob
import subprocess
from datetime import datetime
from typing import Annotated
from autogen import ConversableAgent, GroupChat, GroupChatManager

# --- Config ---

LITELLM_BASE = os.environ.get("LITELLM_BASE", "http://localhost:4000")
LITELLM_KEY = os.environ.get("LITELLM_KEY", "sk-litellm-master-key-local")
DEFAULT_MODEL = os.environ.get("PERF_MODEL", "deepseek-chat")

# --- Tools ---

def read_file(path: Annotated[str, "Path to file to read"]) -> str:
    """Read a file and return its contents."""
    try:
        with open(path) as f:
            content = f.read()
        # Truncate very large files
        if len(content) > 8000:
            return content[:4000] + f"\n\n... ({len(content)} chars total, truncated) ...\n\n" + content[-2000:]
        return content
    except Exception as e:
        return f"Error reading {path}: {e}"


def run_command(command: Annotated[str, "Shell command to execute"]) -> str:
    """Run a shell command and return output. Timeout 120s."""
    try:
        result = subprocess.run(
            command, shell=True, capture_output=True, text=True, timeout=120
        )
        output = result.stdout
        if result.stderr:
            output += f"\nSTDERR: {result.stderr[-500:]}"
        if len(output) > 5000:
            output = output[:2500] + "\n...(truncated)...\n" + output[-1500:]
        return output or "(no output)"
    except subprocess.TimeoutExpired:
        return "Command timed out (120s)"
    except Exception as e:
        return f"Error: {e}"


def write_report(
    path: Annotated[str, "Path to write report"],
    content: Annotated[str, "Markdown content of the report"],
) -> str:
    """Write a markdown report to a file."""
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(content)
        return f"Report written to {path}"
    except Exception as e:
        return f"Error writing report: {e}"


def list_loadtest_runs() -> str:
    """List all available loadtest runs."""
    runs = sorted(glob.glob("loadtest-output/2*"))
    if not runs:
        return "No runs found. Run: ./tests/loadtest/run.sh --soak-minutes 0"
    result = "Available runs:\n"
    for r in runs[-10:]:  # last 10
        result += f"  {r}\n"
    return result


# --- Main ---

def main():
    skip_test = "--skip-test" in sys.argv
    model = DEFAULT_MODEL
    for i, arg in enumerate(sys.argv[1:], 1):
        if arg.startswith("--model="):
            model = arg.split("=", 1)[1]
        elif arg == "--model" and i + 1 < len(sys.argv):
            model = sys.argv[i + 1]

    print(f"Model: {model} (via litellm @ {LITELLM_BASE})")
    print(f"Skip test: {skip_test}")
    print()

    # Suppress logging noise
    import logging
    logging.getLogger("autogen.oai.client").setLevel(logging.ERROR)
    logging.getLogger("autogen").setLevel(logging.WARNING)

    llm_config = {
        "config_list": [{
            "model": model,
            "base_url": f"{LITELLM_BASE}/v1",
            "api_key": LITELLM_KEY,
        }],
        "temperature": 0.1,
    }

    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    report_dir = f"reports/perf/{timestamp}"

    # --- Agent 1: LoadTest Runner ---
    loadtest_runner = ConversableAgent(
        name="loadtest_runner",
        system_message="""Ты LoadTest Runner. Твоя задача — запустить нагрузочный тест и собрать данные.

Доступные инструменты: run_command, list_loadtest_runs.

Шаги:
1. Проверь есть ли свежие результаты: list_loadtest_runs()
2. Если нет или попросили — запусти тест: run_command("./tests/loadtest/run.sh --soak-minutes 0 --plateau-wait 30")
3. После теста прочитай результаты и передай Perf Analyst.

Если тебя попросили пропустить тест — просто найди последний run и передай данные.

Пиши на русском. Будь кратким.""",
        llm_config=llm_config,
        human_input_mode="NEVER",
    )

    # --- Agent 2: Perf Analyst ---
    perf_analyst = ConversableAgent(
        name="perf_analyst",
        system_message=f"""Ты Performance Analyst. Ты анализируешь результаты НТ и исходный код.

Доступные инструменты: read_file, run_command.

Шаги:
1. Прочитай данные теста:
   - run_command("cat <run_dir>/hw-info.json")
   - run_command("tail -1 <run_dir>/loadtest-results.jsonl")
   - run_command("tail -1 <run_dir>/peer-metrics.jsonl")

2. Для каждой медленной операции (>100ms) найди причину в коде:
   - read_file("src/graph_runtime.rs") — nREPL command dispatch
   - read_file("src/db.rs") — DB queries
   - read_file("src/actor.rs") — compute pipeline

3. Передай Report Writer:
   - Top-3 bottleneck с file:line
   - DB queries per compute
   - Memory trend
   - Конкретные рекомендации

Пиши на русском. Всегда указывай конкретные файлы и строки.""",
        llm_config=llm_config,
        human_input_mode="NEVER",
    )

    # --- Agent 3: Report Writer ---
    report_writer = ConversableAgent(
        name="report_writer",
        system_message=f"""Ты Report Writer. Пишешь финальный отчёт по результатам анализа.

Доступный инструмент: write_report.

На основе данных от LoadTest Runner и Perf Analyst напиши отчёт в:
  write_report("{report_dir}/analysis.md", content)

Структура отчёта:
1. Железо и окружение
2. Throughput (ops/sec, клиенты)
3. Per-op breakdown (таблица с %, file:line)
4. DB нагрузка (queries/compute, avg ms)
5. Memory (RSS trend, env cache, утечки)
6. Приоритизированные рекомендации (HIGH/MEDIUM/LOW)

После записи скажи "REPORT_DONE" чтобы завершить.

Пиши на русском.""",
        llm_config=llm_config,
        human_input_mode="NEVER",
    )

    # --- Register tools ---
    for agent in [loadtest_runner, perf_analyst, report_writer]:
        agent.register_for_llm(name="read_file", description="Read a file")(read_file)
        agent.register_for_llm(name="run_command", description="Run shell command")(run_command)
        agent.register_for_llm(name="write_report", description="Write markdown report")(write_report)
        agent.register_for_llm(name="list_loadtest_runs", description="List loadtest runs")(list_loadtest_runs)

    # All agents can execute all tools
    for agent in [loadtest_runner, perf_analyst, report_writer]:
        agent.register_for_execution(name="read_file")(read_file)
        agent.register_for_execution(name="run_command")(run_command)
        agent.register_for_execution(name="write_report")(write_report)
        agent.register_for_execution(name="list_loadtest_runs")(list_loadtest_runs)

    # --- Group Chat ---
    def is_done(msg):
        return msg.get("content") and "REPORT_DONE" in msg.get("content", "")

    group_chat = GroupChat(
        agents=[loadtest_runner, perf_analyst, report_writer],
        messages=[],
        max_round=20,
        speaker_selection_method="auto",
    )

    manager = GroupChatManager(
        groupchat=group_chat,
        llm_config=llm_config,
        is_termination_msg=is_done,
    )

    # --- Kick off ---
    if skip_test:
        initial_msg = """Пропускаем тест.
1. loadtest_runner: найди последний run через list_loadtest_runs()
2. perf_analyst: прочитай данные и исходный код, найди bottleneck'и
3. report_writer: напиши отчёт"""
    else:
        initial_msg = """Полный цикл:
1. loadtest_runner: запусти тест через run_command("./tests/loadtest/run.sh --soak-minutes 0 --plateau-wait 30")
2. perf_analyst: после теста прочитай данные и код
3. report_writer: напиши отчёт"""

    # Use a human proxy to start the conversation
    human = ConversableAgent("human", human_input_mode="NEVER", max_consecutive_auto_reply=0)

    print("=" * 60)
    print("  AG2 Performance Pipeline")
    print("  Agents: loadtest_runner → perf_analyst → report_writer")
    print("=" * 60)
    print()

    human.initiate_chat(manager, message=initial_msg)

    print()
    print("=" * 60)
    print(f"  Report: {report_dir}/analysis.md")
    print("=" * 60)


if __name__ == "__main__":
    main()
