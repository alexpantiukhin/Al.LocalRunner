---
name: up-runner
description: >-
  Use to start the hub, the broker and the runners of this machine. Default: one general runner.
  Triggers: "up runner", "start runners", "подними раннер", "запусти раннеры".
---

# up-runner

Inputs: the runner settings file (default: one general runner) and the number of copies (`--count`, default 1).

1. If `localrunner` is not installed, run `install-runner` first. Read `../../shared/os/<windows|linux|macos>.md` for the current OS.
2. A runner with its own settings or several copies is created once: `localrunner runner join <host:port> --code-file <file> --config <runner.toml> --count N --name <name>`. The code comes from `localrunner broker add` on the broker. Pass it with `--code-file` (`-` is standard input) or `LOCALRUNNER_JOIN_CODE`; avoid the command line, other local users see it. Never print or store the code.
3. `localrunner up` starts every role that `init` created, each as a background process, in the order broker, hub, runners. `localrunner up hub|broker|runner|all` starts one role. Running roles are not duplicated. Do not add another detaching wrapper.
4. A broker installed as an OS service is not started by `up`: start it with the service manager. If `up` returns 1, report the reason it printed.
5. Finish with `status-runner`; `localrunner runners` shows whether the runners reported in.
