---
name: down-runner
description: >-
  Use to stop local runners, the hub and the broker. Triggers: "down runner", "stop runners",
  "выключи раннер", "останови раннеры".
---

# down-runner

Inputs: runner names (or `hub`, `broker`), `--drain` or `--cancel`. A runner is named as `localrunner runners` prints it: `<name>` or `local:<name>` (the prefix is the broker's).

1. Ask only if jobs are active and the mode is not given: `--drain` (default: running jobs finish, then the runner stops) or `--cancel` (running jobs are cancelled; waiting for the runners to exit is limited to 30 seconds).
2. `localrunner down [names] [--drain|--cancel --yes]`. `--cancel` asks for confirmation and is refused without a terminal: the user chose it in step 1, so add `--yes`. Without names it stops every role of this installation, the broker last so the results reach it. `--wait [--wait-timeout 90s]` returns only when the stopped runners have exited (default limit 60 s) and names those that did not, code 1; it also holds the hub until it has no running or waiting order (nothing is stopped and the code is 1 when orders are left): use it before removing or replacing the state directory, and not as the default, because a drain lasts as long as the job. Ctrl+C interrupts only the waiting: runners keep draining, the broker stays up, the code is 1.
3. A broker installed as an OS service is not stopped by `down`: its admin channel is open only to an administrator. For a running service `down broker` says so and prints the restart command to run from an elevated shell (code 1 for a named broker; a bare `down` prints it and keeps code 0); for an installed service that is not running it says that. Never run the elevated command without the user's go-ahead. A runner installed as an OS service that `down` cannot reach (Windows) is named with the stop command for an administrator, code 1; report it, never run it without the go-ahead. A named role that is not running gives code 1.
4. Read `../../shared/os/<windows|linux|macos>.md` for the `ci` runner: after stopping it wait about 30 seconds before starting it again, or GitHub rejects the session as a conflict.
5. Never stop processes this plugin did not start.
6. A stopped runner comes back with `localrunner up <name>` (see `up-runner`), without touching the others.
7. Finish with `status-runner`.
