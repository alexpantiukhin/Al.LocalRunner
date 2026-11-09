---
name: install-runner
description: >-
  Use once per machine to install Al.LocalRunner: the local daemon (light and heavy workers) and,
  if absent, the GitHub Actions self-hosted runner for the ci level. Idempotent. Triggers:
  "install runner", "set up local runner", "установи раннер", "настрой раннер".
---

# install-runner

Contract: `docs/design.md` of Al.LocalRunner, section "Skills contract".

1. Detect the OS and read `../../shared/os/<windows|linux|macos>.md` of the plugin; its paths, runner archive, start method and stop method are binding for this skill.
2. Detect what is present: daemon, `ci` runner directory, registration. Report it and stop if everything is present.
3. Install the daemon; create the state directory.
4. For `ci`: ask the user for the repository and a fresh registration token. Never store, log or echo the token. Configure the labels from the OS file in its own directory.
5. Install nothing at user scope; plugins stay project-scoped.
6. Finish with `status-runner`.
