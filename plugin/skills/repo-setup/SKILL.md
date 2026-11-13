---
name: repo-setup
description: >-
  Use to set up Al.LocalRunner in a repository interactively: find what is heavy, write the
  profiles file and the `temp/` ignore line after you confirm each diff, and check the result.
  Triggers: "set up localrunner in this repo", "configure profiles", "настрой localrunner в
  репозитории", "добавь профили раннера", "что тяжёлое запускать через раннер".
---

# repo-setup

Inputs: what to change or what is missing (optional) and the state of the repository.

1. Detect quietly: does `.localrunner/profiles.toml` exist, and is `temp/` in `.gitignore`. Read nothing else yet.
2. New repository: first ask "analyze the repository, or you tell me what is heavy?"; read project files only after the user chooses analysis. Configured repository: first ask what to change or what is missing; offer an audit (`localrunner profile check`, then a read of the profiles file) only when there is no request. Reading project files to find what is missing needs the same consent as in a new repository. Never overwrite or reformat existing entries; add or change only what was asked.
3. Uncover the needs by scenarios: what is heavy, what disturbs parallel sessions, which services the work needs, which secrets. Propose profiles and keep their fields to `reference.md`.
4. Numbers: run `localrunner runners`, show the memory, CPU and platforms of the machine, and ask for `mem_mb`, `cpu` and `timeout_s`. When the user does not know, propose half of the machine and `timeout_s = 900` and label them a guess. Refuse a profile that does not fit the runner and say which limit is exceeded.
5. Mode: ask explicitly "by commit (the profile has `platforms`, the order carries a pushed commit, the runner must declare the platform) or by work tree". Check the platforms of the runner in the output of `localrunner runners`.
6. Write one file at a time. Show the exact diff of `.localrunner/profiles.toml`, wait for confirmation, then write it. Then the same for `.gitignore`: the single line `temp/`, no duplicate. Touch nothing else in a file.
7. Machine side, only when a profile needs it (a service, a secret source, a tag, a program): show the exact diff for `runner.toml` or `policy.toml` and apply it only after explicit consent; verify with `localrunner doctor` and `localrunner policy check`. Before anything that needs elevation ask whether the user is ready for the UAC prompt and wait for the answer. Never set `allow_elevated` or `allow_same_user_as_broker`. Never print, store or ask to paste secret values: a secret source is an `env:` name only.
8. Ending: run `localrunner profile check` (`localrunner profile check --json` when you parse the result, `localrunner profile check --file <path>` for a file elsewhere), then `localrunner doctor`, and fix what they report. Then OFFER a trial `localrunner run <profile> --as <label>` with a clear label such as `claude:repo-setup`; never start it without a yes. The correctness of a profile is decided by `profile check`, not by `reference.md`.
9. Scope: this skill configures only what localrunner owns (`.localrunner/profiles.toml`, `temp/` in `.gitignore`, and on consent `runner.toml` and `policy.toml`) and edits nothing else.
