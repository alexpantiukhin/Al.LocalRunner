---
name: run
description: >-
  Use whenever tests or builds must run on a machine shared by parallel sessions: submit the run
  to the queue instead of starting it directly. Triggers: "run tests via runner", "прогони через
  раннер", and any test-run step of a consumer project that declares profiles.
---

# run

1. Pick the profile from the consumer `.localrunner/profiles.toml`. The level comes from the profile; override only to a lighter one. Never invent a profile.
2. Make sure runners are up (`status-runner`); if not, `up-runner`.
3. Submit the profile from the working directory to test (a worktree is fine). The timeout comes from the profile and may only be lowered. Introduce yourself: `--as <label>` is required. Use `<agent>:<short session name>` (for example `claude:fix-609`) and keep the same label for the whole session; add `--note` saying what the run is for. Never submit under another session label.
4. Wait for the job as a background task; the log goes to `temp/runner/<job-id>.log`. Never read the whole log: bring into context the final state, the exit code and the failing lines.
5. A profile with `steps` is one job: the steps run back to back and the report lists each step as `passed`, `failed`, `timeout` or `skipped`. Name the first failed step; do not rerun the whole sequence to see a later one. Job states: `passed`, `failed`, `timeout`, `cancelled`. A timeout is a failure of the run, not of the tests: report it as such and do not retry in a loop.
6. Know your own jobs: note the job id that `submit` prints, and after a context loss or restart list them with `jobs --mine --active` (or read `temp/runner/*.json`). Never guess which job is yours from the profile name.
   Cancel early when the result no longer matters (the code changed, a step already failed, the user changed the task): `cancel <job-id>` or `cancel --mine`, with `--reason`. A queued job leaves the queue at once; a running one is stopped gracefully and then killed with its whole tree. Interrupting `wait` does not cancel the job. If the session ends, its queued jobs are dropped and running ones killed unless submitted with `--detach`.
7. Do not start the same command outside the queue while a job of your session is queued or running.
8. Do not cancel jobs of other labels, and never use `--force` unless the user asked for it.
