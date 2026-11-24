# Linux

Used by `install-runner`, `up-runner`, `autostart-runner`, `down-runner`. Shell: bash.

| Item | Value |
|---|---|
| State directory | `~/.local/share/al-localrunner` (`XDG_DATA_HOME` is not read; `LOCALRUNNER_STATE_DIR` overrides it); runners in `runner/<name>/` of it; broker service: `/var/lib/al-localrunner` |
| IPC | Unix sockets in `ipc/` of the state directory (closed to the user, 0700): `hub-<user>.sock`, `runner-<user>-<name>.sock`, `broker.sock`, `broker-admin.sock`; broker service: `/run/al-localrunner/broker.sock` (group `al-localrunner`); not `$XDG_RUNTIME_DIR`, which autostart without login, containers and `sudo -u` lack |
| Runner as an OS service | `sudo localrunner autostart on runner <name> --service --state-dir <state>` (the command prints the exact line): system unit `/etc/systemd/system/al-localrunner-runner-<name>.service`, `User=` the installing user, `NoNewPrivileges=true`, `Delegate=yes` (the runner makes the job cgroups of `limit` itself; a unit written by an earlier version lacks it: `autostart off runner <name> --service`, then `on` again); survives logout and starts at boot. A user unit with `loginctl enable-linger` also survives logout. The service runs as the user, so `down` and `reload` reach it (`down` drains it; it then stays stopped). A `systemctl stop` kills running jobs without a drain. `up` does not start a second copy of an installed service runner and `upgrade` does not start it again: both print `sudo systemctl restart al-localrunner-runner-<name>` |
| Process-tree kill | own process group or cgroup, `killpg` then SIGKILL after a grace period |
| Free memory and CPU | `MemAvailable` in `/proc/meminfo`, `nproc` |

## ci runner
1. Download `actions-runner-linux-x64-<ver>.tar.gz` (or `linux-arm64`) from the `actions/runner` releases; unpack into `<state>/github-runners/ci-1`.
2. `./config.sh --url <repo-url> --token <token> --name <host>-ci-1 --labels al-ci --unattended --replace`.
3. Start as a systemd user unit (`~/.config/systemd/user/al-localrunner-ci-1.service`, `ExecStart=<dir>/run.sh`, `Restart=on-failure`), enable with `systemctl --user enable --now`. Run `loginctl enable-linger $USER` only if it must survive logout. A system-wide unit under another user loses the user toolchains and caches.
4. PATH: take it from the login shell (`bash -lc 'echo $PATH'`) and set it in the unit with `Environment=PATH=...`.
5. Ready when `journalctl --user -u al-localrunner-ci-1` shows "Listening for Jobs".

## Stop
`systemctl --user stop al-localrunner-ci-1`; wait about 30 seconds before the next start.

## Notes
- Emulators need KVM: `/dev/kvm` must exist and be writable by the user (group `kvm`); otherwise reject `emulator` tag jobs.
- `/tmp` may be a small tmpfs; the daemon keeps logs under the submitter `temp/` and its state under the data directory, never in `/tmp`.
- A runner in a container (`install/compose.yaml`) is built from the `dockerfile_inline` of its service: the base OS and the packages are one visible list (`debian:bookworm-slim` with `ca-certificates git python3` by default; `alpine` works too), the binary is copied from the published image of the same version (`COPY --from=ghcr.io/alexpantiukhin/al.localrunner:<version>`, an explicit tag), the user and `cap_drop: [ALL]` stay; after a change of the list run `docker compose build runner`. System tools (git, python) belong there, so `[programs]` finds them; heavy toolchains (node, java, rust) come from a mounted directory: the key `env.tools_dir` (`/opt/tools`) appends `bin` and `<name>/bin` of it to `path`. The container reads the `runner.toml` beside `compose.yaml`, mounted read-only as a single file, directly: `runner join` writes no copy of it into the state volume (and refuses `--platforms` and `--tags`: set them in the file), an edit made in place is picked up by `localrunner reload` or a restart, and the limit of memory and cores is Docker's (`mem_limit`, `cpus` of the service), because a container has no systemd and `[limits] enforce = "limit"` runs as `warn`; a running container keeps the file it mounted at its start, while `localrunner config set` writes a new file in place of the old one, so run the command on the host and then recreate the container. In order, when the runner has no job: mount the tools directory (the commented `./tools:/opt/tools:ro` line of `install/compose.yaml`), run `localrunner --state-dir <dir> --config-dir <dir> config set runner:<name> env.tools_dir /opt/tools` with `<dir>` the directory of `compose.yaml` (it prints `not running` for the runner, which is expected), then `docker compose up -d --force-recreate runner` in that directory; a `docker compose restart` is not enough.
- Containers started by a job belong to that job: remove them in the profile cleanup so orphaned ones do not hold ports.
