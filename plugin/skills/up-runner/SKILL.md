---
name: up-runner
description: >-
  Use to start local runners. Default: one light, one heavy and one ci runner. Triggers:
  "up runner", "start runners", "подними раннер", "запусти раннеры".
---

# up-runner

1. If not installed, run `install-runner` first.
2. Start the requested levels (default `light,heavy,ci`) with the requested worker count (default 1). Skip those already running.
3. Start every long-running process as its own background task, with no self-detaching wrapper.
4. The `ci` runner must inherit a PATH that contains the project toolchains; start it from the user session, not as a service.
5. Finish with `status-runner`; wait for "Listening for Jobs" on `ci` before reporting success.
