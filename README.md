# Al.LocalRunner

Local queued runners for tests and builds on one developer machine. Parallel agent sessions submit runs to a shared queue instead of competing for CPU, memory, the emulator and Gradle/cargo caches.

Shipped as a Claude Code plugin (`plugin/`). The core is project-agnostic: it knows nothing about Rust, Kotlin or any consumer. Each consumer project declares its own profiles (see `docs/architecture.html`).

## Requirements instead of levels

There are no levels. A job lists what it needs (platform, memory, CPU, disk, installed programs, tags, timeout); a runner reports what it has; the hub and the broker pick a runner that satisfies all of it, and answer with the exact difference when none does. Priority and ordering come from the owner policy (`policy.toml` on the broker), not from the code.

A single machine runs a hub, a broker and a runner by default, so no configuration is needed. All runners of a pool share one resource budget, so a `ci` job never overlaps with a conflicting local run.

## Skills

| Skill | Purpose |
|---|---|
| `install-runner` | one-time setup of the daemon and the `ci` runner if absent |
| `up-runner` | start the hub, the broker and runners (default: one general runner) |
| `down-runner` | stop runners, drain or cancel the queue |
| `status-runner` | queue, running jobs, waiting time, free resources |
| `run` | how to submit a run: level, profile, timeout, reading the result |

Install into a consumer project with project scope only:
`claude plugin marketplace add <url> --scope project`, then `claude plugin install al-localrunner@al-localrunner --scope project`.

## Status

Skeleton. Contracts of the skills and the queue are in `docs/architecture.html`; implementation is tracked in issue #1.
