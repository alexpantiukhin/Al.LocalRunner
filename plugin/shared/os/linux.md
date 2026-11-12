# Linux

Used by `install-runner`, `up-runner`, `autostart-runner`, `down-runner`. Shell: bash.

| Item | Value |
|---|---|
| State directory | `~/.local/share/al-localrunner` (`XDG_DATA_HOME` is not read; `LOCALRUNNER_STATE_DIR` overrides it); runners in `runner/<name>/` of it; broker service: `/var/lib/al-localrunner` |
| IPC | Unix sockets in `ipc/` of the state directory (closed to the user, 0700): `hub-<user>.sock`, `runner-<user>-<name>.sock`, `broker.sock`, `broker-admin.sock`; broker service: `/run/al-localrunner/broker.sock` (group `al-localrunner`); not `$XDG_RUNTIME_DIR`, which autostart without login, containers and `sudo -u` lack |
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
- Containers started by a job belong to that job: remove them in the profile cleanup so orphaned ones do not hold ports.
