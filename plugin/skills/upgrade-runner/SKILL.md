---
name: upgrade-runner
description: >-
  Use to update Al.LocalRunner on this machine to a newer release: the binary, and the hub, broker
  and runners that run. Triggers: "upgrade runner", "update localrunner", "обнови раннер",
  "обнови localrunner", "новая версия localrunner".
---

# upgrade-runner

Inputs: the target version (default: the latest release).

1. Read `../../shared/os/<windows|linux|macos>.md` (relative to this skill). Then `localrunner --version` and `localrunner status`: tell the user the installed version, what runs and whether a job is active. Nothing installed: use `install-runner` instead.
2. Find the target on the release page (https://github.com/alexpantiukhin/Al.LocalRunner/releases). Same version as the installed one: report it and stop. The major protocol version differs between the two: the broker, the hubs and the runners of other machines must be upgraded in the same window; say so and ask before going on.
3. `upgrade` lists the running jobs and the waiting orders before it asks. With running jobs it offers: wait for them to finish (can take long; Ctrl+C stops waiting and nothing is replaced, the runners keep draining), cancel them (`--cancel`, the runners exit within 30 s), or abort. Ask the user which one; `--yes` alone waits, `--yes --cancel` cancels. Never choose `--cancel` on your own. Waiting orders stay queued.
4. Get the new binary only through the signed installer of the release, never a copy from a branch: `install.sh --upgrade --version X.Y.Z` or `install.ps1 --upgrade --version X.Y.Z` (commands in README.md, section "Installation"). `--upgrade` runs `localrunner upgrade` of the downloaded binary: it prints the installed and the new version, the roles it stops and starts again, asks, stops the runners first (drain or cancel), then the hub and a broker started by hand, copies itself over the installed binary and starts the same roles from it. Nothing is stopped before the user types `yes`. It refuses root or administrator unless `--allow-elevated`; do not pass that flag on your own.
5. The broker as an OS service keeps its own copy of the binary and `upgrade` leaves it alone. When the machine runs one: tell the user, and update the service only with their consent after asking for readiness for the UAC prompt or `sudo` as the user's global rules require (stop the service, replace the copy named in `README.md`, "Update", start it). Do not touch a service the user did not mention.
6. Other machines (a remote runner, a hub on another host) are upgraded one by one with the same steps on each of them. The broker goes first when the protocol version is the same; for a major change follow step 2.
7. After the upgrade: `localrunner --version` shows the new number, then `localrunner doctor` and `status-runner`. If a role did not come back, report which one and the log directory; do not retry in a loop.
8. The plugin itself is updated separately, through the marketplace at project scope (`--scope project`); install nothing at user scope.
9. A failed `upgrade` restarts the roles it stopped from the old binary and says what failed; report it, do not delete files by hand.
