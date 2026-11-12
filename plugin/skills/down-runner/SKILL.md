---
name: down-runner
description: >-
  Use to stop local runners, the hub and the broker. Triggers: "down runner", "stop runners",
  "выключи раннер", "останови раннеры".
---

# down-runner

Inputs: runner names (or `hub`, `broker`), `--drain` or `--cancel`.

1. Ask only if jobs are active and the mode is not given: `--drain` (default: running jobs finish, then the runner stops) or `--cancel` (running jobs are cancelled; waiting for the runners to exit is limited to 30 seconds).
2. `localrunner down [names] [--drain|--cancel --yes]`. `--cancel` asks for confirmation and is refused without a terminal: the user chose it in step 1, so add `--yes`. Without names it stops every role of this installation, the broker last so the results reach it. Ctrl+C interrupts only the waiting: runners keep draining, the broker stays up, the code is 1.
3. A broker installed as an OS service is not stopped by `down`: it answers with a hint; stop the service with the service manager. A named role that is not running gives code 1.
4. Read `../../shared/os/<windows|linux|macos>.md` for the `ci` runner: after stopping it wait about 30 seconds before starting it again, or GitHub rejects the session as a conflict.
5. Never stop processes this plugin did not start.
6. Finish with `status-runner`.
