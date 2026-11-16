# Windows

Used by `install-runner`, `up-runner`, `autostart-runner`, `down-runner`. Shell: PowerShell 7.

| Item | Value |
|---|---|
| State directory | `%LOCALAPPDATA%\Al.LocalRunner` (`LOCALRUNNER_STATE_DIR` overrides it); runners in `runner\<name>\` of it; broker service: `%ProgramData%\Al.LocalRunner` |
| IPC | named pipes `\\.\pipe\al-localrunner-hub-<SID>`, `al-localrunner-runner-<SID>-<name>`, `al-localrunner-broker`, `al-localrunner-broker-admin`; the broker service listens on `al-localrunner-broker` too, open to `[[local_user]]` and administrators |
| Runner as an OS service | `autostart on runner <name> --service` (administrator): service `AlLocalRunner-<name>` under `NT SERVICE\AlLocalRunner-<name>`, which gets modify on the state directory and read on the configuration and binary directories; only a runner joined with its own key (`runner join`) whose key belongs to the installing user (another administrator is refused); the Administrators group takes the key (`takeown /A`, it stays the owner) and makes the service account its only entry until `autostart off`, which gives it back to the user named by the service's `--owner` and checks the result (a failure prints the `takeown`/`icacls` commands to run); after `on` the key is owned by the Administrators group with the service account as its only entry, after `off` owned by the Administrators group with the user as its only entry (both checks accept it; no restore privilege needed); `on` and `off` change or remove only a service whose configuration (full program path, `runner run --name <name> --service`, `--state-dir`, account) is this installation's; `on` checks that the service keeps running for a few seconds and otherwise names `logs/service.log` and fails; `off` waits for the stop and names any directory right it could not take back; a failed re-run never removes an existing service and says whether its definition changed; known limitations: modify on the whole state directory, and the state directory is compared as text (an 8.3 name, a junction, `..` or an extended path counts as another installation); a local-channel runner is refused; the account lacks the user PATH and per-user tools, so jobs see only tools installed for all users; `down`/`reload`/`upgrade` do not reach it and print `Stop-Service`/`Restart-Service AlLocalRunner-<name>` to run as administrator; a service stop kills running jobs without a drain |
| Process-tree kill | Job Object with kill-on-close |
| Free memory and CPU | `Win32_OperatingSystem.FreePhysicalMemory`, logical processor count |

## ci runner
1. Download `actions-runner-win-x64-<ver>.zip` (or `win-arm64`) from the `actions/runner` releases; unpack into `%LOCALAPPDATA%\Al.LocalRunner\github-runners\ci-1`.
2. `config.cmd --url <repo-url> --token <token> --name <host>-ci-1 --labels al-ci --unattended --replace`. The default labels already include `self-hosted`, `Windows`, `X64`.
3. Start from the user session, not as a service: `Start-Process <dir>\run.cmd -WorkingDirectory <dir> -WindowStyle Hidden`. A service runs in another session and loses the user PATH, the user `~/.cargo`, `~/.gradle`, ADB and the interactive desktop.
4. Put the toolchain `bin` directories on PATH before `Start-Process`; the runner inherits the environment of its parent.
5. Ready when the newest `_diag\Runner_*.log` contains "Listening for Jobs".

## Stop
Stop `Runner.Listener` and the `run.cmd` process of that runner directory only. Wait about 30 seconds before the next start, or GitHub answers `TaskAgentSessionConflictException`.

## Notes
- Prevent sleep while a job runs (`SetThreadExecutionState`), release it when idle.
- Emulators need WHPX or Hyper-V; check before accepting an `emulator` tag job.
- Allow the daemon through nothing: it listens on the pipe only, no firewall rule is needed.
- Git Bash users: do not set `MSYS_NO_PATHCONV=1` globally.
