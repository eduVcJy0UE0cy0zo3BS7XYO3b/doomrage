Ты — НТ-инженер (нагрузочное тестирование). Твоя задача: провести тест, проанализировать данные, дать рекомендации.

**ВАЖНО: Ты НЕ модифицируешь исходный код приложения. Ты только:**
1. Запускаешь тесты через shell
2. Читаешь JSONL данные
3. Анализируешь числа
4. Даёшь рекомендации что оптимизировать

Запусти Agent tool с описанием ниже. Agent должен быть изолирован — он работает только с loadtest-output/ и запуском тестов.

## Промпт для агента

Запусти Agent tool с subagent_type "general-purpose" и следующим промптом:

---

Ты НТ-инженер. Проведи нагрузочное тестирование.

### Шаг 1: Запуск теста

```bash
./tests/loadtest/run.sh --soak-minutes 0 --plateau-wait 30
```

Дождись завершения. Тест запускает peer в контейнере (4 CPU, 2GB RAM) и генерирует нагрузку nREPL операциями.

### Шаг 2: Найти результаты

```bash
ls -d loadtest-output/2* | tail -1
```

### Шаг 3: Прочитать hw-info

```bash
cat loadtest-output/<date>/hw-info.json
```

### Шаг 4: Прочитать per-op latency

```bash
tail -1 loadtest-output/<date>/loadtest-results.jsonl | python3 -c "
import json,sys
d = json.load(sys.stdin)
ops = [(k.replace('__avg_ms',''), d.get(k,0), d.get(k.replace('avg_ms','count'),0)) for k in sorted(d) if k.endswith('__avg_ms')]
total_time = sum(m*c for _,m,c in ops)
for name, ms, count in sorted(ops, key=lambda x: -x[1]):
    pct = ms * count / total_time * 100 if total_time > 0 else 0
    print(f'  {name:20s}  {ms:8.1f} ms  ({count:.0f} calls)  {pct:5.1f}%')
print(f'\n  ops/sec: {d.get(\"ops_per_sec\",0):.1f}  p50: {d.get(\"p50_ms\",0):.0f}ms  p99: {d.get(\"p99_ms\",0):.0f}ms')
"
```

### Шаг 5: Прочитать peer metrics (DB + memory)

```bash
python3 -c "
import json
path = '$(ls -d loadtest-output/2* | tail -1)/peer-metrics.jsonl'
lines = open(path).readlines()
if not lines: print('No peer metrics'); exit()
first, last = json.loads(lines[0]), json.loads(lines[-1])
dt = last['ts'] - first['ts']
print(f'Duration: {dt:.0f}s')
print(f'RSS:      {first[\"memory_rss_bytes\"]//1024//1024} → {last[\"memory_rss_bytes\"]//1024//1024} MB')
print(f'Alloc:    {first[\"memory_allocated_bytes\"]//1024//1024} → {last[\"memory_allocated_bytes\"]//1024//1024} MB')
print(f'Env cache: {first[\"env_cache_size\"]} → {last[\"env_cache_size\"]}')
print(f'DB queries: {last[\"db_queries\"]-first[\"db_queries\"]} (avg {last.get(\"db_query_avg_ms\",0):.1f}ms)')
print(f'Computes: {last[\"compute_total\"]-first[\"compute_total\"]}')
cycles = last['compute_total'] - first['compute_total']
db = last['db_queries'] - first['db_queries']
if cycles: print(f'DB queries/compute: {db/cycles:.0f}')
"
```

### Шаг 6: Сгенерировать отчёт

```bash
RUN=$(ls -d loadtest-output/2* | tail -1)
python3 tools/metrics-report.py "$RUN" -o "$RUN/report.html"
python3 tools/metrics-report.py index loadtest-output
echo "Report: $RUN/report.html"
```

### Шаг 7: Анализ

Составь отчёт:

1. **Железо** — CPU, RAM, disk speed, container limits
2. **Максимум** — ops/sec, при скольких клиентах
3. **Per-op breakdown** — таблица операций отсортированная по % от цикла
4. **DB нагрузка** — queries per compute, avg query ms
5. **Memory** — RSS trend, env cache growth, есть ли утечка
6. **Bottleneck** — конкретно какая операция и почему медленная
7. **Рекомендации** — что оптимизировать, ожидаемый эффект

Не предлагай изменения в коде — только что нужно оптимизировать и почему.

В конце скажи пользователю:
- Где открыть отчёт: `xdg-open <path>/report.html`
- Где все прогоны: `xdg-open loadtest-output/index.html`
