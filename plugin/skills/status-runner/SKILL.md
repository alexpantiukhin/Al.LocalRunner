---
name: status-runner
description: >-
  Use to see the queue: running and waiting jobs with steps, waiting time and reason, runners and
  free resources, as text, JSON, an HTML snapshot or a live page. Triggers: "runner status",
  "что в очереди", "статус раннера", "покажи очередь в html".
---

# status-runner

Read only; changes nothing. Read commands take `--json`.

- `localrunner status`: queue and running jobs with steps and the wait reason. Filters: `--project`, `--source session|github`, `--as <label>`, `--broker <name>`.
- `status --html [path]`: one self-contained HTML file, default `temp/runner/queue.html`; add `--history` for the history table. `--with-logs` embeds job logs, which may hold secrets of profile commands: use it only when the user asks. Give the user the printed path.
- `localrunner explain <job-id>`: why a job waits, naming the policy rule.
- `localrunner runners [--broker B]`: state, platforms, programs, pool load, tags, services. `localrunner brokers`: known brokers, availability, orders in the outbox.
- `localrunner history [--project --source --as --profile --result --since --limit --platform --runner --broker]`; `--all` shows every run the visibility policy allows; `history show <job-id>` is the full record. `localrunner stats [--project --profile --client --since]` for pass rate, waiting time and saturation.
- `localrunner log <job-id> [--step NAME] [--tail N] [-f]`: read with `--tail` or a step, never whole.
- `localrunner dashboard`: live page on loopback only (127.0.0.1 and ::1). Start it as its own background task, give the user the URL from its output, stop it when the user is done. Never expose it beyond loopback.
- If nothing answers, run `localrunner doctor`.
