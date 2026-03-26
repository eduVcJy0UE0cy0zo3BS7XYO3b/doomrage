Проведи нагрузочное тестирование wasm-canvas peer.

## Workflow

### 1. Запуск теста

Два режима:
- **find-max** — найти максимальное количество клиентов: `make loadtest-find-max`
- **soak** — держать N клиентов час: `make loadtest-soak MAX_CLIENTS=N`

Для быстрого теста можно:
```bash
./tests/loadtest/run.sh --soak-minutes 5 --max-clients 2
```

### 2. Мониторинг

Пока тест идёт, следи за выводом:
- `p50` / `p99` — медиана и 99-й перцентиль latency
- `err` — процент ошибок
- `ops` — общее количество операций
- `cycles` — завершённые полные циклы (create → compute → delete)

Ключевая метрика: **ops/sec** — операций в секунду.

Критерии полочки (plateau):
- ops/sec перестал расти при добавлении клиентов — нашли максимум
- Подтверждение: ждёт `--plateau-wait` секунд (default 60) и перемеряет

Hard limits:
- **p99 > 1000ms** — система перегружена
- **error rate > 1%** — система нестабильна
- **p99 растёт монотонно** — утечка памяти или ресурсов

### 3. Просмотр отчёта

После теста:
```bash
xdg-open loadtest-output/index.html
```

Каждый прогон сохраняется в `loadtest-output/YYYYMMDD-HHMMSS/`.
Индексная страница показывает все прогоны с summary.

Прочитай HTML отчёт конкретного прогона через Read tool:
```
loadtest-output/<date>/report.html
```

### 4. Анализ результатов

Посмотри на графики в отчёте и проанализируй:

**Latency (p50/p99):**
- Плоская линия = стабильно
- Растущая = деградация, возможно утечка или GC pressure
- Скачки = конкуренция за ресурсы

**Throughput:**
- Сколько cycles/sec при максимальной нагрузке?
- Деградирует ли throughput со временем?

**Errors:**
- 0% — идеально
- < 1% — приемлемо для production
- > 1% — нужна оптимизация

### 5. Рекомендации

На основе результатов предложи:
- Если p99 высокий — какой компонент bottleneck (scheme-rs init, SurrealDB, nREPL thread pool)?
- Если memory растёт — где утечка?
- Конкретные изменения в коде для улучшения

### Метрики peer

Во время теста peer также пишет `peer-metrics.jsonl`. Посмотри через:
```bash
python3 tools/metrics-report.py loadtest-output/<date>/peer-metrics.jsonl -o /tmp/peer-report.html
```

Это покажет: compute duration, definitions count, pending computes — со стороны сервера.

## Файлы

- `tests/loadtest/run.sh` — скрипт запуска (podman)
- `tests/loadtest/src/main.rs` — loadtest binary (Rust)
- `tools/metrics-report.py` — генератор HTML отчётов
- `loadtest-output/` — результаты всех прогонов
- `loadtest-output/index.html` — индекс прогонов
