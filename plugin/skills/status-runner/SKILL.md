---
name: status-runner
description: >-
  Use to see the runner queue: running and waiting jobs with their steps, waiting time and reason,
  free memory and CPU, as text, JSON, an HTML snapshot or a live page. Triggers: "runner status",
  "что в очереди", "статус раннера", "покажи очередь в html".
---

# status-runner

Read only; changes nothing.

- Default: text, per level: workers up or down, running jobs (id, profile, submitter directory, elapsed vs timeout, steps with state), queued jobs (position, waiting time, why it waits), held resource tags, free memory and CPU.
- Each job is shown with its project, source (`session` or `github`, with the run link) and submitter label. Filters: `--project`, `--source`, `--as`.
- `--json`: the same data for scripts.
- `--html [path]`: one self-contained HTML file, default `temp/runner/queue.html`. Give the user its full path.
- History: `history` (filters `--project`, `--source`, `--as`, `--profile`, `--result`, `--since`, `--limit`), `history show <job-id>`, `log <job-id> [--step NAME] [--tail N]`, and `stats` for pass rate, waiting time and duration per profile. Read the log with `--tail` or a step, never whole.
- `dashboard`: interactive page (Queue, History, Stats, Runners tabs; filters, live log, steps timeline, cancel with confirmation) on `127.0.0.1` with a per-start token; `?as=<label>` opens it focused on your own jobs. Start it as its own background task, give the user the URL from its output, and stop it when the user is done. Never expose it beyond loopback.
