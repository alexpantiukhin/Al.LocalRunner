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
- `--json`: the same data for scripts.
- `--html [path]`: one self-contained HTML file, default `temp/runner/queue.html`. Give the user its full path.
- `dashboard`: live page on `127.0.0.1` with a per-start token. Start it as its own background task, give the user the URL from its output, and stop it when the user is done. Never expose it beyond loopback.
