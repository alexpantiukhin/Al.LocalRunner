---
name: autostart-runner
description: >-
  Use to make the hub, the broker or runners start with the OS, or to stop that. Triggers:
  "autostart runner", "start runners at login", "автозапуск раннера", "включи автозапуск".
---

# autostart-runner

Inputs: `on` or `off`, the roles (`hub`, `broker`, `runner`, `all`; default: the roles `init` created on this machine), optionally one runner name.

1. Read `../../shared/os/<windows|linux|macos>.md`. Show the actions first: `localrunner autostart on|off [role] --dry-run`.
2. Apply: `localrunner autostart on|off [hub|broker|runner|all]`. One runner: `localrunner autostart on|off runner <name>`. The command is idempotent.
3. `on` only enables autostart; it starts nothing now. Start the roles with `up-runner`. A manual `up` keeps working.
4. `autostart off broker` stops and removes the broker OS service (administrator rights needed; the state stays). Installing the service is `localrunner broker install-service` (administrator): ask the user first, never elevate on your own.
5. Finish with `localrunner doctor` and report its findings.
