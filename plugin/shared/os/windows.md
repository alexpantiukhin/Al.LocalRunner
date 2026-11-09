# Windows

Used by `install-runner`, `up-runner`, `down-runner`. Shell: PowerShell 7.

| Item | Value |
|---|---|
| State and runner directories | `%LOCALAPPDATA%\Al.LocalRunner` |
| IPC | named pipe `\\.\pipe\al-localrunner` |
| Process-tree kill | Job Object with kill-on-close |
| Free memory and CPU | `Win32_OperatingSystem.FreePhysicalMemory`, logical processor count |

## ci runner
1. Download `actions-runner-win-x64-<ver>.zip` (or `win-arm64`) from the `actions/runner` releases; unpack into `%LOCALAPPDATA%\Al.LocalRunner\runners\ci-1`.
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
