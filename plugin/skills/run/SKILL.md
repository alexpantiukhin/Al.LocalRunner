---
name: run
description: >-
  Use whenever tests or builds must run on a machine shared by parallel sessions: submit the run
  to the queue instead of starting it directly. Triggers: "run tests via runner", "прогони через
  раннер", and any test-run step of a consumer project that declares profiles.
---

# run

Inputs: the sender label (`--as`), an optional note, the profile, an optional lower timeout.

1. Pick the profile from the consumer `.localrunner/profiles.toml`; with a single profile the name may be omitted. Never invent a profile. Requirements (memory, CPU, programs, tags) come from the profile; `--mem <size>` (`6144`, `512m`, `6g`) and `--cpu <cores>` replace memory and CPU for this run only.
2. Make sure the roles are up (`status-runner`); if not, `up-runner`.
3. Submit from the working directory to test (a worktree is fine): `localrunner submit [profile] --as <label> [--note ...] [--timeout s]`, or `localrunner run ...`, which is `submit --wait` and returns the job's exit code. `--as` is required: use `<agent>:<short session name>` (for example `claude:fix-609`) and keep it for the whole session; never submit under another session label. `--timeout` (seconds) only lowers the profile timeout. `--platform <name>` makes a commit order for one other platform.
4. `submit` prints the job id. Wait as a background task with `localrunner wait <job-id>` or follow `localrunner log <job-id> -f`; write the output to `temp/runner/<job-id>.log`. Never read the whole log: bring into context the final state, the exit code and the failing lines (`log <job-id> --tail N`, `--step NAME`). Interrupting `wait` cancels nothing.
5. A profile with `steps` is one job; the report lists each step. Job results: `passed`, `failed`, `timeout`, `cancelled`, `lost`. Name the first failed step; do not rerun the whole sequence to see a later one. A timeout or `lost` is a failure of the run, not of the tests: report it as such and do not retry in a loop. If a job waits, run `localrunner explain <job-id>`.
6. Know your own jobs: after a context loss list them with `localrunner jobs --mine --active --as <label>`. Never guess which job is yours from the profile name. Cancel early when the result no longer matters: `localrunner cancel <job-id> --as <label> --reason ...` or `cancel --mine --as <label>`. Add `--detach` to `submit` only when the run must outlive the sending process.
7. Do not start the same command outside the queue while a job of your session is queued or running.
8. Do not cancel jobs of other labels, and never use `--force` unless the user asked for it.
