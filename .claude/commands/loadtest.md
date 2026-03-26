Проведи нагрузочное тестирование wasm-canvas peer.

## Что делать

1. Запусти тест
2. Дождись завершения
3. Прочитай данные из JSONL
4. Проанализируй результаты
5. Дай рекомендации по оптимизации

## 1. Запуск

Запусти find-max (поиск максимума):
```bash
./tests/loadtest/run.sh --soak-minutes 0 --plateau-wait 30
```

Или soak (держать нагрузку):
```bash
./tests/loadtest/run.sh --max-clients N --soak-minutes 60
```

## 2. После завершения — прочитай данные

Найди последний прогон:
```bash
ls -d loadtest-output/2* | tail -1
```

Прочитай последнюю строку loadtest-results.jsonl через bash:
```bash
tail -1 loadtest-output/<date>/loadtest-results.jsonl
```

Прочитай последнюю строку peer-metrics.jsonl:
```bash
tail -1 loadtest-output/<date>/peer-metrics.jsonl
```

Посчитай тренд памяти (первая и последняя строка peer-metrics):
```bash
head -1 loadtest-output/<date>/peer-metrics.jsonl
tail -1 loadtest-output/<date>/peer-metrics.jsonl
```

## 3. Что анализировать

### Per-op latency (из loadtest-results.jsonl)

Поля `create_node__avg_ms`, `update_node__avg_ms`, `compute__avg_ms`, `node_state__avg_ms`, `defs__avg_ms`, `info__avg_ms`, `delete_node__avg_ms`.

Отсортируй по убыванию — самая медленная операция = bottleneck.

### DB нагрузка (из peer-metrics.jsonl)

- `db_queries` — сколько всего SQL запросов
- `db_query_avg_ms` — средняя latency запроса
- Посчитай queries per nREPL cycle = db_queries / cycles

### Memory (из peer-metrics.jsonl)

- `memory_rss_bytes` — RSS в байтах, сравни первую и последнюю строку
- `memory_allocated_bytes` — jemalloc allocated
- `env_cache_size` — кешированные Scheme environments

Если RSS растёт линейно — утечка. Если env_cache растёт но ноды удаляются — кеш не очищается.

### Throughput

- `ops_per_sec` — операций в секунду
- При добавлении клиента ops/sec должен расти. Если падает — contention.

## 4. Рекомендации

На основе данных предложи конкретные изменения:

- **node-state медленный** → читать через with_graphs напрямую вместо command channel
- **update-node медленный** → batch DB queries в save_node_definitions
- **DB avg > 10ms** → batch queries, или перейти на HashMap вместо SurrealDB для hot path
- **RSS растёт** → проверить env_cache cleanup при delete-node
- **env_cache растёт** → очищать при delete-node

## 5. Отчёт

Сгенерируй HTML отчёт:
```bash
python3 tools/metrics-report.py loadtest-output/<date>/ -o loadtest-output/<date>/report.html
```

Скажи пользователю где открыть: `xdg-open loadtest-output/<date>/report.html`

Все прогоны: `xdg-open loadtest-output/index.html`
