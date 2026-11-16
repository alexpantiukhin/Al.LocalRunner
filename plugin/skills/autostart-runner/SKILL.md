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
3. `on` only enables autostart; it starts nothing now. Start the roles with `up-runner`. A manual `up` keeps working. Hub and runner autostart starts at user login: it stops at logout and does not start after a reboot until someone logs in.
4. A runner that must survive logout and reboots: `localrunner autostart on runner <name> --service` makes it an OS service (never root or `SYSTEM`; the OS file names the service). It needs administrator rights: show `--dry-run` first, ask the user, never elevate on your own; without rights the command prints the exact line to run (with `--state-dir`), give that line to the user. The `ci` runner and runners that need a desktop session are refused: report the refusal, do not work around it. `autostart off runner <name> --service` removes only that service.
5. `autostart off broker` stops and removes the broker OS service (administrator rights needed; the state stays). Installing the service is `localrunner broker install-service` (administrator): ask the user first, never elevate on your own.
6. Finish with `localrunner doctor` and report its findings; a `runner <name> autostart` warning "starts at login only" means step 4 is the fix if the runner must keep running.
