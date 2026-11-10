---
name: install-runner
description: >-
  Use once per machine to install Al.LocalRunner: the localrunner binary, the state directory and,
  if absent, the GitHub Actions self-hosted runner registered as the `ci` runner. Idempotent.
  Triggers: "install runner", "set up local runner", "установи раннер", "настрой раннер".
---

# install-runner

Inputs: the repository for the `ci` runner (optional) and a fresh registration token (ask for it, never store, log or echo it).

1. Detect the OS and read `../../shared/os/<windows|linux|macos>.md` (relative to this skill); its paths, runner archive, start and stop methods are binding.
2. Detect what is present: `localrunner --version`, the state directory, the `ci` runner directory. If everything is present, report it and stop.
3. Install the binary for the current user: `localrunner install` (show the actions first with `--dry-run`; `--dir <dir>` chooses the directory, `--role hub|broker|runner|all` limits the roles). It refuses root or administrator unless `--allow-elevated` is given; do not pass that flag on your own. It starts no roles and enables no autostart.
4. Run `localrunner init` if the state directory has no templates, then `localrunner doctor` and show what it reports.
5. `ci` runner: only when a repository is given and no `ci` runner exists. Ask for the token and configure the GitHub Actions runner in its own directory with the labels from the OS file.
6. Install nothing at user scope; plugins stay project-scoped (`--scope project`).
7. Finish with `status-runner`.
