---
name: autostart-runner
description: >-
  Use to make the hub, the broker or runners start with the OS, or to stop that. Triggers:
  "autostart runner", "start runners at login", "автозапуск раннера", "включи автозапуск".
---

# autostart-runner

Inputs: `on` or `off`, the roles (`hub`, `broker`, `runner`, `all`; default: the roles `init` created on this machine), optionally one runner name.

1. Read `../../shared/os/<windows|linux|macos>.md`. Show the actions first: `localrunner autostart on|off [role] --dry-run`.
2. Apply: `localrunner autostart on|off [hub|broker|runner|all]`. `autostart on` without a role enables only the hub and the runners; the broker (an OS service under its own account, administrator rights) is installed only by `autostart on broker` or `autostart on all`. One runner: `localrunner autostart on|off runner <name>`. The command is idempotent.
3. To see the current state without changing anything: `localrunner autostart status [role] [--json]` (no rights needed); run it first and report what is already on. `on` only enables autostart; it starts nothing now. Start the roles with `up-runner`. A manual `up` keeps working. Hub and runner autostart starts at user login: it stops at logout and does not start after a reboot until someone logs in.
4. A runner that must survive logout and reboots: `localrunner autostart on runner <name> --service` makes it an OS service (never root or `SYSTEM`; the OS file names the service). It needs administrator rights: show `--dry-run` first, ask the user, never elevate on your own; in the user's terminal the command asks for the rights itself (UAC or `sudo`) after its own question, which only the user answers; without a terminal or with `--no-elevate` it prints the exact line to run (with `--state-dir`), give that line to the user. The `ci` runner, runners that need a desktop session, a runner missing from the installation, a runner that also starts at login, and on Windows a runner without its own broker key (not joined with `runner join`) are refused: report the refusal, do not work around it; for the last one the refusal and `doctor` name the way (`broker add --runners 1`, `runner join <address> --name <name>`, then `autostart on runner <name> --service` again), offer it to the user. A service stop kills running jobs without a drain. A service does not see the user's environment: a secret of a service runner is `from = "file:/absolute/path"` (the command grants the Windows service account read access to the file and takes it back on `off`; the value is never printed), and an `env:` secret is warned about. The command prints its service notes (no user `PATH`, the computer account on the network, no drain) and the checks of `runner.toml` (an `env:` secret the service cannot see, a `docker` tag) before the rights are asked for and in `--dry-run`: read them to the user. `autostart off runner <name> --service` removes only that service.
5. `autostart off broker` stops and removes the broker OS service (administrator rights needed, asked for in the user's terminal; the state stays). Installing the service is `localrunner broker install-service` (administrator): ask the user first, never elevate on your own.
6. Finish with `localrunner doctor` and report its findings; a `runner <name> autostart` warning "starts at login only" means step 4 is the fix if the runner must keep running.
