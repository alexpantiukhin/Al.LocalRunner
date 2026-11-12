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
- `localrunner explain <job-id>`: why a job waits, naming the policy rule. A wait reason `занято чужими процессами` means the runner machine has too little free memory or CPU because of foreign load (broker policy `[admission]`); the job starts by itself when the load drops. For a finished job it also gives the end reason and the peaks.
- `localrunner runners [--broker B]`: state, platforms, programs, pool load, tags, services, and `enforcement` (`os`: limits of the OS; `watcher`: user-space sampling, approximate on macOS). `localrunner brokers`: known brokers, availability, orders in the outbox.
- `localrunner history [--project --source --as --profile --result --since --limit --platform --runner --broker]`; `--all` shows every run the visibility policy allows; `history show <job-id>` is the full record. Every finished job has its peak memory and disk in MB (`PEAK_MEM_MB`, `PEAK_DISK_MB` of `history`, `peak_mem_mb` and `peak_disk_mb` in `--json` and in `status --json`, `peak mem` lines of `history show`); use them to judge whether the `mem_mb` and `disk_mb` of a profile are set right. A job killed for exceeding its declared `mem_mb` or `disk_mb` ends `failed` with the reason `resource_exceeded` and the text "declared X MB, peak Y MB" (only when the machine owner set `[resources] mode = "kill"` in the broker policy; without the table the default is `warn`: the log holds a warning, the CPU is capped and the process count is limited; with an explicit `off` nothing is limited and only the peaks are recorded). Fix such a job by raising the declared value in the profile, not by retrying. An order the broker refused at intake (more than the pool, a name no runner list has, a secret not allowed for the label) is recorded too: `history` shows it as `rejected` (`history --result rejected` lists them) and `history show` gives the reason; fix the request, not the broker. `localrunner stats [--project --profile --client --since]` for pass rate, waiting time and saturation.
- `localrunner log <job-id> [--step NAME] [--tail N] [-f]`: read with `--tail` or a step, never whole.
- `localrunner dashboard`: live page on loopback only (127.0.0.1 and ::1). Start it as its own background task, give the user the URL from its output, stop it when the user is done. Never expose it beyond loopback.
- If nothing answers, run `localrunner doctor`.
