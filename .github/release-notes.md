**Added:** `localrunner runner cache clean [NAME] [--older-than DAYS] [--dry-run]` frees the named caches of a running runner; `runners` shows why a runner below its `[accept] min_free_disk_mb` takes no orders and `doctor` warns before the disk is full.

**Added:** `upgrade` finds outdated `localrunner` plugins in Claude Code and Codex and offers to update them through the agent CLI, each in its own scope; it warns about the service update and its exit code reflects a failed update.

**Added:** `wait` prints the reason an order waits and one line per change to standard error; a step that exits with code 127 gets a hint (list the program in `programs` of the profile) in the output of `wait` and `run` and in `explain`.

**Fixed:** roles that `upgrade` starts again through a systemd unit or a launchd agent no longer stay in drain (a runner on a VPS did not come back after `upgrade`).

**Fixed:** `doctor` and `secret list` check `env:` secrets against the environment of the running runner process (names only), else its unit, else the shell, and the text says which; the false `not set in the runner process` warning is gone.

**Changed:** the skills and both READMEs describe the above. Flaky `dashboard_live` test waits for the running state.
