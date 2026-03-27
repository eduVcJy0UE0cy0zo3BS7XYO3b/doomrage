# Как изменится daily workflow с multi-agent системой

## Текущий workflow

```
Утро:
  1. Открываешь Claude Code
  2. Описываешь задачу
  3. Claude пишет код, тесты, фиксит
  4. Ты ревьюишь, правишь
  5. Коммит, пуш
  6. (иногда) запускаешь /loadtest, /perf
  7. Повторяешь

Проблемы:
  - Claude и пишет и ревьюит — конфликт интересов
  - Забываешь запустить тесты / security review
  - Нет автоматической проверки после пуша
  - Всё последовательно — ты ждёшь пока Claude думает
```

## Переходный workflow (ближайший шаг)

```
Claude Code (ты за клавиатурой)
  ├── Пишешь фичу → коммит → пуш
  │
  └── Git push триггерит CI pipeline:
        ├── cargo test (автоматически)
        ├── AG2 Code Review Agent (дешёвая модель — gemma/llama)
        │     → комментарии на PR
        ├── AG2 Security Agent (Claude — дорогая но точная)
        │     → security findings
        └── (по кнопке) /loadtest → /perf
              → performance отчёт

Ты продолжаешь работать в Claude Code.
Через 2-5 минут получаешь ревью на PR.
```

### Что меняется:
- **Code review** — больше не ты и не Claude Code. Отдельный агент.
- **Security** — автоматически на каждый PR.
- **Performance** — по запросу, но данные копятся.
- **Код** — ты всё ещё пишешь через Claude Code. Это не меняется.

### Что нужно настроить:
1. GitHub Actions / CI pipeline
2. AG2 с litellm (чтобы review на дешёвой модели)
3. Webhook: PR → запуск агентов

## Зрелый workflow (через месяц-два)

```
Ты: "Нужна фича X" (описание в issue)
  │
  ├── AG2 Planner Agent → декомпозиция задачи
  │     → создаёт sub-issues
  │
  ├── Claude Code (ты) → реализуешь
  │     → или Codex Agent (автоматически, простые задачи)
  │
  ├── Git push →
  │     ├── Code Review Agent (автоматически)
  │     ├── Test Agent → запускает тесты, анализирует coverage
  │     ├── Security Agent
  │     └── Performance Agent (если затронуты hot paths)
  │
  └── Всё зелёное → auto-merge (или ручной approve)

Параллельно (по расписанию):
  ├── Weekly loadtest (soak 1 час) → отчёт в Slack/email
  ├── Daily security scan
  └── Мониторинг RSS/DB в production
```

### Что меняется:
- **Простые задачи** (рефакторинг, миграция, документация) — агент делает сам, ты ревьюишь
- **Сложные задачи** — ты пишешь, агенты проверяют
- **Quality gates** — автоматические, не забудешь
- **Мониторинг** — непрерывный, не по запросу

## Конкретный пример: один день

### Сейчас:
```
09:00  Открыл Claude Code
09:05  "Добавь batch DB queries в register_def"
09:30  Claude написал код, ты проверил
09:35  Коммит, пуш
09:36  Забыл запустить тесты
10:00  Вспомнил, cargo test — всё ок
10:05  Забыл про security review
11:00  Другая задача...
```

### С агентами:
```
09:00  Открыл Claude Code
09:05  "Добавь batch DB queries в register_def"
09:30  Claude написал код
09:31  Пуш → CI запускается автоматически
09:33  Пришёл code review от агента: "LGTM, но проверь edge case когда defines пустой"
09:34  Пришёл security review: "OK, SQL injection safe — batch через prepared statement"
09:35  cargo test passed
09:36  → auto-merge
09:37  Следующая задача — ни минуты потерянной

11:00  Пришёл ежедневный performance отчёт: "После batch fix: 3.5 → 12 ops/sec (+3.4x)"
```

## Стоимость

| Агент | Модель | Стоимость/запуск | Частота |
|-------|--------|------------------|---------|
| Code Review | Gemma 27B (Ollama, бесплатно) | $0 | Каждый PR |
| Security Review | Claude Haiku | ~$0.01 | Каждый PR |
| Test Analysis | Local LLM | $0 | Каждый PR |
| Performance | Claude Sonnet | ~$0.05 | По запросу |
| Weekly Soak Test | Compute only | ~$0 (свой CPU) | 1 раз/неделю |

**Total: ~$0.01-0.06 на PR.** При 10 PR/день = $0.10-0.60/день.

## С чего начать (первый шаг)

1. **Поставить AG2**: `pip install ag2`
2. **Настроить litellm**: один config для всех моделей
3. **Написать code-review агент**: ~50 строк Python
4. **Подключить к GitHub Actions**: PR → агент → комментарий
5. Всё. Остальное добавлять постепенно.
