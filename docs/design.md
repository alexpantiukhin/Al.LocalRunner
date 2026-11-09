# Al.LocalRunner — design

## Goals
- Parallel agent sessions queue their runs instead of overloading the machine.
- Every run has a mandatory timeout; the whole process tree is killed on expiry or cancel.
- The core is generic; consumers supply profiles.

## Architecture

```mermaid
flowchart LR
  S1[Session A] -->|submit| D
  S2[Session B] -->|submit| D
  subgraph D[Local daemon]
    Q[FIFO queue per level] --> SCH[Resource scheduler]
  end
  SCH --> L[light worker]
  SCH --> H[heavy worker]
  GH[GitHub Actions] --> CI[ci runner]
  CI -->|acquire| SCH
```

- `light` and `heavy` are workers of the local daemon. A run executes in the submitter working directory, so uncommitted changes are tested.
- `ci` is a real GitHub Actions self-hosted runner (label `al-ci`). Before executing a job it acquires its resource tags from the same scheduler through the daemon, so it never overlaps a conflicting local run.
- One worker per level by default. More workers per level are allowed; the scheduler enforces the limits.

## Profiles (consumer-supplied)
`.localrunner/profiles.toml` in the consumer repository:

```toml
[profile.rust-unit]
level = "light"
command = "cargo test --workspace"
timeout_s = 900
mem_mb = 4096
cpu = 4
tags = ["cargo"]

[profile.android-tier]
level = "heavy"
command = "pwsh scripts/android-tier.ps1"
timeout_s = 3600
mem_mb = 12288
cpu = 6
tags = ["gradle", "emulator"]
exclusive = ["gradle", "emulator"]
```

- `timeout_s` is required; there is no default and no unbounded run. The daemon enforces an upper cap from its own config.
- `tags` are resources held for the duration of the run; `exclusive` lists tags that no other running job may hold.
- The core never interprets `command`; it only spawns it.

## Queue and scheduling
- FIFO per level; a job starts when a worker of its level is free and the scheduler grants memory, CPU and tags.
- A job that can never fit the machine limits is rejected on submit with the reason.
- States: `queued` → `running` → `passed | failed | timeout | cancelled`.
- Each job writes stdout and stderr to `temp/runner/<job-id>.log` in the submitter directory; the submit call returns the job id, and waiting streams progress and returns the exit code.
- Cancel and timeout kill the process tree (Windows Job Object; process group elsewhere).
- Crash safety: a dead submitter queued jobs are dropped, a dead job is reaped, leases are tied to the daemon child processes.

## Transport and state
- Local IPC only: named pipe on Windows, Unix socket elsewhere. No network listener.
- State and runner directories under `%LOCALAPPDATA%\Al.LocalRunner` (`~/.local/share/al-localrunner` elsewhere).
- The `ci` runner lives in its own directory and registers per repository with a token supplied by the user at install time; the token is never stored by the plugin.

## Skills contract
| Skill | Inputs | Result |
|---|---|---|
| `install-runner` | repository for `ci` (optional), registration token (asked, not stored) | daemon installed; `ci` runner configured if absent; idempotent |
| `up-runner` | levels (default `light,heavy,ci`), workers per level (default 1) | runners running; prints status |
| `down-runner` | levels, `--drain` or `--cancel` | runners stopped; queue handled as asked |
| `status-runner` | none | queue, running jobs, waiting time, free resources |
| `run` | profile, level override, timeout override (lowered only) | job id, log path, exit code |

## Out of scope
- Phones and physical devices over USB/ADB.
- Any consumer-specific command or toolchain knowledge inside the core.
