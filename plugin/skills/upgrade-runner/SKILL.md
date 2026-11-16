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
3. `upgrade` lists the running jobs and the waiting orders before it asks. With running jobs it offers: wait for them to finish (can take long; Ctrl+C stops waiting and nothing is replaced, the runners keep draining), cancel them (`--cancel`, the runners exit within 30 s), or abort. Ask the user which one; `--yes` alone waits, `--yes --cancel` cancels. Never choose `--cancel` on your own. Waiting orders stay queued. With `--cancel` it prints `cancelling: <jobs>` (only the jobs of the runners it stops), `still running elsewhere: <jobs>` when runners of other machines run jobs (it does not stop them), and `staying queued: <orders>` (every queued order); afterwards `localrunner status --recent` shows the cancelled jobs as `cancelled by upgrade: runner_stopped`.
4. Start with `localrunner upgrade [--version X.Y.Z]` from the installed binary: it downloads the release, checks the signature and the SHA-256 itself against the keys built into the binary, never a copy from a branch (`--offline ARCHIVE` takes the files from a folder). It prints the installed and the new version, the roles it stops and starts again, asks, stops the runners first (drain or cancel), then the hub and a broker started by hand, replaces the installed binary and starts the same roles from it. While it runs, `localrunner up` refuses to start runners of this installation; if `up` or `status` says an upgrade runs, do not repeat it, wait. The signed installer (`install.sh --upgrade --version X.Y.Z` or `install.ps1 --upgrade --version X.Y.Z`, commands in README.md, section "Installation") stays for the first installation and for a machine without a working binary. Nothing is stopped before the user types `yes`. It refuses root or administrator unless `--allow-elevated`; do not pass that flag on your own.
5. The broker as an OS service keeps its own copy of the binary and `upgrade` leaves it alone. When the machine runs one: tell the user, and update the service only with their consent after asking for readiness for the UAC prompt or `sudo` as the user's global rules require: `localrunner broker update-service --dry-run` shows the plan, and without administrator rights the real command prints the exact line to run and never elevates itself (`README.md`, "Updating the broker service"). Do not touch a service the user did not mention.
6. Other machines (a remote runner, a hub on another host) are upgraded one by one with the same steps on each of them. The broker goes first when the protocol version is the same; for a major change follow step 2.
7. After the upgrade: `localrunner --version` shows the new number, then `localrunner doctor` and `status-runner`. If a role did not come back, report which one and the log directory; do not retry in a loop.
8. The plugin itself is updated separately, through the marketplace at project scope (`--scope project`); install nothing at user scope.
9. A failed `upgrade` restarts the roles it stopped from the old binary and says what failed; report it, do not delete files by hand.
