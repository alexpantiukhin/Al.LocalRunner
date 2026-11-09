# Al.LocalRunner

Local queued runners for tests and builds on one developer machine. Parallel agent sessions submit runs to a shared queue instead of competing for CPU, memory, the emulator and Gradle/cargo caches.

Shipped as a Claude Code plugin (`plugin/`). The core is project-agnostic: it knows nothing about Rust, Kotlin or any consumer. Each consumer project declares its own profiles (see `docs/design.md`).

## Levels

| Level | Backend | Purpose | Sees uncommitted tree |
|---|---|---|---|
| `light` | any runner | single tests during work on one feature | yes on the submitter machine, no on a remote runner (pushed commit) |
| `heavy` | any runner | whole test tiers, emulator runs | yes on the submitter machine, no on a remote runner (pushed commit) |
| `ci` | GitHub Actions self-hosted runner | CI jobs and deploys | no (pushed commit) |

Levels are labels defined by the machine owner in `levels.toml` (priority and the characteristics a runner must have: memory, CPU, free disk, installed programs); the three above are the defaults, so a single machine needs no file. A different gradation is a different file.

By default one runner per level is started. All levels share one resource scheduler, so a `ci` job never overlaps with a conflicting `heavy` run.

## Skills

| Skill | Purpose |
|---|---|
| `install-runner` | one-time setup of the daemon and the `ci` runner if absent |
| `up-runner` | start runners (default: one `light`, one `heavy`, one `ci`) |
| `down-runner` | stop runners, drain or cancel the queue |
| `status-runner` | queue, running jobs, waiting time, free resources |
| `run` | how to submit a run: level, profile, timeout, reading the result |

Install into a consumer project with project scope only:
`claude plugin marketplace add <url> --scope project`, then `claude plugin install al-localrunner@al-localrunner --scope project`.

## Status

Skeleton. Contracts of the skills and the queue are in `docs/design.md`; implementation is tracked in issue #1.
