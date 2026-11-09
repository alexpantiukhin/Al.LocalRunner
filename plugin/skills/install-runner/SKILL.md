---
name: install-runner
description: >-
  Use once per machine to install Al.LocalRunner: the local daemon (light and heavy workers) and,
  if absent, the GitHub Actions self-hosted runner for the ci level. Idempotent. Triggers:
  "install runner", "set up local runner", "установи раннер", "настрой раннер".
---

# install-runner

Contract: `docs/design.md` of Al.LocalRunner, section "Skills contract".

1. Detect what is present: daemon, `ci` runner directory, registration. Report it and stop if everything is present.
2. Install the daemon; create the state directory.
3. For `ci`: ask the user for the repository and a fresh registration token. Never store, log or echo the token. Configure labels `self-hosted`, the OS and `al-ci`, in its own directory.
4. Install nothing at user scope; plugins stay project-scoped.
5. Finish with `status-runner`.
