# macOS

Used by `install-runner`, `up-runner`, `autostart-runner`, `down-runner`. Shell: zsh or bash.

| Item | Value |
|---|---|
| State directory | `~/.local/share/al-localrunner` (`LOCALRUNNER_STATE_DIR` overrides it); runners in `runner/<name>/` of it; only the broker service uses `/Library/Application Support/Al.LocalRunner` |
| IPC | Unix sockets in `ipc/` of the state directory (closed to the user, 0700): `hub-<user>.sock`, `runner-<user>-<name>.sock`, `broker.sock`, `broker-admin.sock`; broker service: `ipc/broker.sock` of its state directory (group `_allocalrunner`); the path must stay under the 104-byte socket limit |
| Process-tree kill | own process group, `killpg` then SIGKILL after a grace period |
| Free memory and CPU | `vm_stat` (free + inactive + speculative pages), `sysctl -n hw.ncpu` |

## ci runner
1. Download `actions-runner-osx-arm64-<ver>.tar.gz` (Apple Silicon) or `osx-x64` (Intel) from the `actions/runner` releases; unpack into `<state>/github-runners/ci-1`.
2. Clear quarantine: `xattr -dr com.apple.quarantine <dir>`.
3. `./config.sh --url <repo-url> --token <token> --name <host>-ci-1 --labels al-ci --unattended --replace`.
4. Start as a LaunchAgent in the user GUI domain (`~/Library/LaunchAgents/al.localrunner.ci-1.plist`, `ProgramArguments` = `<dir>/run.sh`, `KeepAlive`, `EnvironmentVariables` with PATH), load with `launchctl bootstrap gui/$(id -u) <plist>`. Do not use a LaunchDaemon: it runs outside the user session and loses the keychain, the toolchains and the simulators.
5. Ready when the newest `_diag/Runner_*.log` contains "Listening for Jobs".

## Stop
`launchctl bootout gui/$(id -u)/al.localrunner.ci-1`; wait about 30 seconds before the next start.

## Notes
- Run the daemon under `caffeinate -i` while a job is active so the machine does not sleep.
- Android emulators on Apple Silicon use arm64 system images with HVF; an x86 image will not run. Xcode and simulator jobs are the consumer profile concern, not the core.
- Gatekeeper may block freshly downloaded binaries: the `xattr` step above is required for the runner and for any binary the daemon installs.
