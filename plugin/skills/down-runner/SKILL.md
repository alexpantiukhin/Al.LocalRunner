---
name: down-runner
description: >-
  Use to stop local runners. Triggers: "down runner", "stop runners", "выключи раннер",
  "останови раннеры".
---

# down-runner

1. Ask only if the queue is non-empty and the mode is not given: `--drain` (finish queued jobs, accept none) or `--cancel` (kill running jobs and drop the queue).
2. Read `../../shared/os/<windows|linux|macos>.md` and stop the requested levels (default all) the way it prescribes.
3. After stopping `ci`, wait about 30 seconds before starting it again, or GitHub rejects the session as a conflict.
4. Never stop processes this plugin did not start.
5. Finish with `status-runner`.
