---
name: up-runner
description: >-
  Use to start the hub, the broker and the runners of this machine. Default: one general runner.
  Triggers: "up runner", "start runners", "подними раннер", "запусти раннеры".
---

# up-runner

Inputs: the runner settings file (default: one general runner) and the number of copies (`--count`, default 1).

1. If `localrunner` is not installed, run `install-runner` first. Read `../../shared/os/<windows|linux|macos>.md` for the current OS.
2. A runner with its own settings or several copies is created once: `localrunner runner join <host:port> --code-file <file> --config <runner.toml> --count N --name <name>`. The code comes from `localrunner broker add` on the broker. Pass it with `--code-file` (`-` is standard input) or `LOCALRUNNER_JOIN_CODE`; avoid the command line, other local users see it. Never print or store the code. If `broker add` is refused because the broker is an OS service (its admin channel is open only to an administrator, on Unix to the broker's OS user) and `join` refuses an administrator, ask the user whether they are ready to confirm the elevation prompt (UAC or `sudo`), name what will run and wait for the answer: `localrunner broker add --runners N --code-file <file>`, with `<file>` in a new private directory (the file must not exist). Run only that command elevated, then `runner join` as the ordinary user, and delete the file and its directory at once, also when the join fails.
3. `localrunner up` starts every role that `init` created, each as a background process, in the order broker, hub, runners. `localrunner up hub|broker|runner|all` starts one role. Running roles are not duplicated. Do not add another detaching wrapper.
4. A broker installed as an OS service is not started by `up`: start it with the service manager. If `up` returns 1, report the reason it printed. A runner refused because it would run under the OS user of a broker that admits other clients (spec 3.12) is reported to the user as is; do not add `allow_same_user_as_broker = true` to `runner.toml` without the owner's explicit decision.
5. Finish with `status-runner`; `localrunner runners` shows whether the runners reported in.
