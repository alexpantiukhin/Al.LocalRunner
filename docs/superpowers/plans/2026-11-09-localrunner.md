# Al.LocalRunner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the whole system of `docs/architecture.html`: a queued runner (daemon, CLI, runners, clients, history, dashboard) as one Rust binary `al-localrunner`.

**Architecture:** A Cargo workspace. Pure logic (model, scheduler, queue selection) lives in `lr-core` with no IO and is tested first; process execution, storage, wire protocol, TLS, daemon, runner agent and dashboard are separate crates over it. One binary, three roles (hub, runner, client). A single machine runs its own hub with local runners; multi-machine is the same code with a network channel.

**Tech Stack:** Rust stable, tokio, serde, toml, clap, rusqlite (bundled SQLite), rustls + tokio-rustls + rcgen, ed25519-dalek, sha2, axum (dashboard), windows-sys (Job Object), libc (process groups).

**Spec:** `docs/architecture.html`

## Global Constraints

- Language Rust; one workspace under `crates/`; the CLI binary is `al-localrunner`.
- Third-party crates only with a free commercial-use license (MIT, Apache 2.0, BSD, ISC; MPL-2.0 unmodified); SQLite is public domain. Enforced by `cargo deny`.
- No comments in code except doc comments on public items; every file under 500 lines, split by responsibility.
- Generic core: daemon, queue and scheduler never name or branch on a consumer, language, toolchain, test framework or platform. Platform names are opaque labels compared for equality.
- `timeout_s` is required on every profile and every step; no default, no unbounded run; the daemon enforces an upper cap from its config.
- Nothing leaves the machine: no telemetry, no external requests, no uploads. The only outbound connections are the `ci` runner to GitHub, a runner to the hub it joined, and `git fetch` on a runner.
- The dashboard binds to `127.0.0.1` and `::1` only, with no option to widen it; the runner channel is a separate listener, active only in hub mode with at least one registered machine, on an explicitly configured address, never a wildcard.
- Remote jobs test a pushed commit; the profile is read from that commit. Local runners run in the submitter directory.
- Temporary files go to `temp/` (gitignored); never read or print files under `secrets/`.
- Commits in Russian, conventional prefix, no `Co-Authored-By` and no agent attribution; plugins project scope only.
- Defaults from the spec: grace before kill 10 s, heartbeat 15 s and offline after 3 missed, join code single use and valid 1 hour, hub-lost grace 10 min, anti-starvation 30 min, retention 30 days and 2 GB logs and 365 days rows.

## Review Focus

- Hub restart with queued and running jobs: queued jobs survive, running jobs of a dead daemon are reaped, nothing is run twice (Task 10).
- Output that is not UTF-8, or one line of 10 MB without a newline: the log and the live stream neither crash nor grow unbounded (Task 6).
- A command that spawns grandchildren which detach or ignore the stop signal: the whole tree is gone before resources are released (Task 5).
- Windows paths with spaces and non-ASCII characters, and `origin` in `https`, `ssh://` and scp-style forms with and without `.git`: project name and checkout path are correct (Tasks 8, 16).
- Cancel racing with finish, and a result or log chunk delivered twice after a reconnect: the final state is unique and history has one row (Tasks 10, 15).

---

### Task 1: Workspace, license gate, tooling

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `deny.toml`, `crates/lr-core/`, `crates/lr-exec/`, `crates/lr-store/`, `crates/lr-proto/`, `crates/lr-net/`, `crates/lr-daemon/`, `crates/lr-runner/`, `crates/lr-dashboard/`, `crates/lr-cli/` (each with `Cargo.toml` and `src/lib.rs`; `lr-cli` has `src/main.rs`)
- Modify: `.gitignore` (add `target/`)

**Interfaces:**
- Produces: workspace members above; `lr-cli` depends on all others; `lr-core` depends on no other workspace crate.

- [ ] **Step 1:** Create the workspace and empty crates; `lr-cli` `main` parses `--version` through clap and prints the crate version.
- [ ] **Step 2:** Write `deny.toml` allowing only MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, MPL-2.0, Unicode-3.0, Unlicense, CC0-1.0 and the SQLite public-domain exception; deny unknown registries.
- [ ] **Step 3:** Run `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo deny check licenses && cargo run -p lr-cli -- --version`. Expected: all pass, version printed.
- [ ] **Step 4:** Commit `chore: каркас workspace и проверка лицензий`.

### Task 2: Core model and profile parsing

**Files:**
- Create: `crates/lr-core/src/{model.rs,profile.rs,error.rs}`
- Test: `crates/lr-core/tests/profile.rs`

**Interfaces:**
- Produces: `JobId(String)`, `GroupId(String)`, `enum Level {Light, Heavy, Ci}`, `enum Zone {Local, Lan, Remote}` (ordered), `enum JobState {Queued, Running, Passed, Failed, Timeout, Cancelled}`, `enum StepState {Queued, Running, Passed, Failed, Timeout, Skipped, Cancelled}`, `struct StepSpec {name, command, timeout_s, mem_mb: Option<u32>, cpu: Option<u32>, tags, continue_on_failure}`, `struct Profile {name, level, platforms: Vec<String>, timeout_s, mem_mb, cpu, tags, exclusive, steps: Vec<StepSpec>, max_zone: Option<Zone>}`, `fn parse_profiles(toml: &str) -> Result<Vec<Profile>, ProfileError>`, `fn resolve_for_platform(p: &Profile, platform: &str) -> Result<Profile, ProfileError>` (applies `[profile.X.platform.P]` override).

- [ ] **Step 1: Write failing tests** in `tests/profile.rs`: `command_and_steps_together_rejected`, `missing_timeout_rejected` (profile and step), `single_command_becomes_one_step`, `platform_override_replaces_command`, `exclusive_must_be_subset_of_tags_is_not_required` (an `exclusive` tag outside `tags` is accepted and unioned), `unknown_level_rejected`, and a parse of the two profiles from the spec asserting `mem_mb == 12288` for `android-tier`.
- [ ] **Step 2:** Run `cargo test -p lr-core --test profile`; expected FAIL (items undefined).
- [ ] **Step 3:** Implement with serde + toml; validation errors carry the profile name and field. Job-level resources are derived later by the scheduler task, not here.
- [ ] **Step 4:** Run the tests; expected PASS. Commit `feat(core): модель и разбор профилей`.

### Task 3: Scheduler (pools, grants)

**Files:**
- Create: `crates/lr-core/src/scheduler.rs`
- Test: `crates/lr-core/tests/scheduler.rs`

**Interfaces:**
- Consumes: `Profile`, `JobId` (Task 2).
- Produces: `struct PoolId(String)`, `struct PoolLimits {mem_mb: u32, cpu: u32, exclusive: BTreeSet<String>}`, `struct Request {mem_mb, cpu, tags: BTreeSet<String>, exclusive: BTreeSet<String>}`, `fn Request::from_profile(&Profile) -> Request` (max over steps for memory and CPU, union of tags and `exclusive`), `enum Reject {ExceedsPool, UnknownPool}`, `enum Wait {Memory, Cpu, ExclusiveTag{tag: String, holder: JobId}}`, `struct Scheduler`, `Scheduler::new(Vec<(PoolId, PoolLimits)>)`, `can_ever_fit(&self, &PoolId, &Request) -> Result<(), Reject>`, `try_grant(&mut self, &PoolId, &JobId, &Request) -> Result<(), Wait>`, `release(&mut self, &JobId)`, `free(&self, &PoolId) -> (u32, u32)`.

- [ ] **Step 1: Write failing tests:** `grant_then_release_restores_free`, `memory_exhaustion_waits_with_memory_reason`, `exclusive_tag_blocks_any_other_holder_and_names_holder`, `tag_held_shared_when_not_exclusive`, `request_larger_than_pool_never_fits`, `two_runners_one_pool_share_limits` (two grants from different jobs on one `PoolId` sum), `release_unknown_job_is_noop`.
- [ ] **Step 2:** Run `cargo test -p lr-core --test scheduler`; expected FAIL.
- [ ] **Step 3:** Implement as pure bookkeeping, no clocks and no IO; acquisition is all-or-nothing.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(core): планировщик ресурсов с пулами`.

### Task 4: Queue selection, priority, routing, zones

**Files:**
- Create: `crates/lr-core/src/queue.rs`, `crates/lr-core/src/zone.rs`
- Test: `crates/lr-core/tests/queue.rs`

**Interfaces:**
- Consumes: Tasks 2–3.
- Produces: `struct NetworkLabel(String)`, `struct MachineId(String)`, `struct RunnerView {id: String, machine: MachineId, network: NetworkLabel, pool: PoolId, levels, platforms, tags, free_slots: u32, online: bool}`, `struct QueueEntry {job: JobId, level: Level, platform: Option<String>, tags, request: Request, submitter_machine: MachineId, submitter_network: NetworkLabel, max_zone: Option<Zone>, enqueued_at: Instant}`, `struct QueueConfig {starvation: Duration}`, `fn zone_of(runner: &RunnerView, entry: &QueueEntry) -> Zone`, `fn pick_next(entries: &[QueueEntry], runners: &[RunnerView], sched: &Scheduler, now: Instant, cfg: &QueueConfig) -> Option<(JobId, String)>` returning job id and runner id; `fn reject_reason(entry, runners) -> Option<String>` for jobs no registered runner can ever match.

- [ ] **Step 1: Write failing tests:** `ci_goes_before_older_light`, `fifo_inside_equal_priority`, `non_ci_waiting_over_starvation_limit_is_ordered_as_ci`, `no_preemption_modelled` (a running job is not in the input and is never returned), `nearest_zone_preferred_local_then_lan_then_remote`, `busy_nearest_spills_to_farther_free_runner`, `max_zone_lan_never_picks_remote`, `non_matching_near_runner_loses_to_matching_far`, `zone_is_relative_to_submitter_machine_and_network`, `equal_zone_picks_most_free_capacity`, `blocked_head_does_not_block_startable_later_job`, `never_matchable_job_gets_reject_reason`.
- [ ] **Step 2:** Run `cargo test -p lr-core --test queue`; expected FAIL.
- [ ] **Step 3:** Implement `pick_next` as one ordered scan: sort by (effective priority, `enqueued_at`), take the first entry with an eligible runner (level, platform, tags, online, free slot) whose pool grants `can_ever_fit` and a dry-run grant; among eligible runners order by `zone_of` then free capacity. `zone_of`: same `MachineId` is Local, equal `NetworkLabel` is Lan, else Remote.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(core): выбор следующего джоба и зоны`.

### Task 5: Process execution, tree kill, timeouts

**Files:**
- Create: `crates/lr-exec/src/{spawn.rs,tree_windows.rs,tree_unix.rs,lib.rs}`
- Test: `crates/lr-exec/tests/tree.rs`

**Interfaces:**
- Produces: `struct ExecSpec {command: String, cwd: PathBuf, env: Vec<(String,String)>}`, `enum StopReason {Exited(i32), Timeout, Cancelled}`, `struct RunningProc` with `async fn wait(&mut self) -> StopReason`, `async fn stop(&mut self, grace: Duration)` (graceful signal, then kill the whole tree after `grace`), `fn spawn(spec: &ExecSpec, timeout: Duration, out: tokio::sync::mpsc::Sender<Vec<u8>>) -> io::Result<RunningProc>`. The command runs through the platform shell (`cmd /C` or `sh -c`); on Windows inside a Job Object with kill-on-close, on Unix as a process-group leader.

- [ ] **Step 1: Write failing tests** with a helper binary or shell snippets: `timeout_kills_child_and_reports_timeout`, `cancel_graceful_then_kill_after_grace` (child ignores the signal; asserted gone within `grace + 2 s`), `grandchild_detached_is_killed_with_tree`, `exit_code_propagates`, `stdout_and_stderr_are_both_captured_in_order_per_stream`, `paths_with_spaces_and_cyrillic_cwd_work`.
- [ ] **Step 2:** Run `cargo test -p lr-exec --test tree`; expected FAIL.
- [ ] **Step 3:** Implement per OS behind one `fn kill_tree(&mut self)`; Windows uses `windows-sys` Job Object and `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` for the graceful stop; Unix uses `libc::killpg` with SIGTERM then SIGKILL.
- [ ] **Step 4:** Run on the current OS; expected PASS. Commit `feat(exec): запуск процессов и убийство дерева`.

### Task 6: Job runner — steps and logs

**Files:**
- Create: `crates/lr-exec/src/{job.rs,log.rs}`
- Test: `crates/lr-exec/tests/job.rs`

**Interfaces:**
- Consumes: `Profile`, `StepSpec`, `StepState`, `JobState`, `spawn` (Tasks 2, 5).
- Produces: `struct StepResult {name, state: StepState, exit_code: Option<i32>, started, finished}`, `struct JobOutcome {state: JobState, steps: Vec<StepResult>}`, `enum JobEvent {StepStarted(String), Chunk{seq: u64, bytes: Vec<u8>}, StepFinished(StepResult)}`, `async fn run_job(profile: &Profile, cwd: &Path, job_timeout: Duration, grace: Duration, cancel: tokio::sync::watch::Receiver<bool>, events: tokio::sync::mpsc::Sender<JobEvent>) -> JobOutcome`, `struct LogWriter` appending to a file with a header per step and a bounded line buffer (`MAX_LINE = 64 KiB`, longer lines are split, invalid UTF-8 kept as bytes).

- [ ] **Step 1: Write failing tests:** `failed_step_skips_the_rest`, `continue_on_failure_keeps_going_job_still_failed`, `step_timeout_is_min_of_step_and_remaining_job`, `cancel_marks_running_cancelled_and_rest_skipped`, `log_has_header_per_step`, `non_utf8_output_does_not_crash_and_is_preserved`, `ten_megabyte_line_without_newline_is_split_and_memory_bounded`, `chunks_carry_increasing_seq`.
- [ ] **Step 2:** Run `cargo test -p lr-exec --test job`; expected FAIL.
- [ ] **Step 3:** Implement sequentially over steps; a step is killed at the earlier of its own and the remaining job timeout.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(exec): выполнение шагов и логи`.

### Task 7: Store — history, logs, retention

**Files:**
- Create: `crates/lr-store/src/{schema.rs,store.rs,retention.rs}`
- Test: `crates/lr-store/tests/store.rs`

**Interfaces:**
- Produces: `struct JobRecord {id, group, project, source, label, note, branch, directories, profile, level, platform, runner, pool, commit, queued_at, started_at, finished_at, state, exit_code, cancelled_by, cancel_reason, run_url, steps: Vec<StepResult>}`, `struct HistoryFilter {project, source, label, profile, state, since, limit}`, `Store::open(dir: &Path) -> Result<Store>`, `upsert(&self, &JobRecord)` (idempotent by id and state transition; a final state is never overwritten by another final state), `get(&self, &JobId)`, `list(&self, &HistoryFilter)`, `append_log(&self, &JobId, &[u8])`, `read_log(&self, &JobId, step: Option<&str>, tail: Option<usize>)`, `prune(&self, cfg: &Retention, now) -> PruneReport` (oldest first, never active jobs), `stats(&self, project, profile) -> Vec<ProfileStats>` (runs, pass rate, median and p95 of wait and duration, timeout and cancel share).

- [ ] **Step 1: Write failing tests:** `upsert_twice_same_final_state_keeps_one_row`, `second_final_state_does_not_overwrite_first`, `list_filters_by_project_state_and_since`, `prune_by_age_and_by_size_removes_oldest_first`, `prune_never_removes_active`, `stats_median_and_p95`, `log_tail_returns_last_n_lines`.
- [ ] **Step 2:** Run `cargo test -p lr-store`; expected FAIL.
- [ ] **Step 3:** Implement with rusqlite (bundled), WAL mode; logs under `logs/<job-id>.log` in the state directory.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(store): история джобов, логи, хранение`.

### Task 8: Submitter identity and source adapters

**Files:**
- Create: `crates/lr-core/src/identity.rs`, `crates/lr-daemon/src/source/{mod.rs,session.rs,github.rs}`
- Test: `crates/lr-core/tests/identity.rs`

**Interfaces:**
- Produces: `fn normalize_origin(url: &str) -> Option<String>` (`owner/repo` from `https://host/owner/repo(.git)`, `ssh://git@host/owner/repo.git`, `git@host:owner/repo.git`), `fn project_name(origin: Option<&str>, repo_root: &Path, dir: &Path) -> String`, `struct Identity {label, note, os_user, host, pid, pid_start, cwd, branch, worktree_root, project, source: Source, machine: MachineId}`, `enum Source {Session, Github(GithubInfo)}`, `fn collect_session(label: &str, note: Option<&str>, cwd: &Path) -> Result<Identity>` (rejects a missing label), `fn collect_github(env: &BTreeMap<String,String>, cwd: &Path) -> Result<Identity>` (label defaults to `github:<workflow>/<job>`, requires `GITHUB_ACTIONS=true`).

- [ ] **Step 1: Write failing tests:** `origin_forms_normalize_equal` (all five forms of one repo), `no_remote_uses_repo_root_name`, `outside_git_uses_directory_name`, `session_without_label_rejected`, `note_over_200_chars_rejected`, `github_env_fills_run_url_and_default_label`, `windows_path_with_spaces_and_cyrillic_project_name`.
- [ ] **Step 2:** Run `cargo test -p lr-core --test identity`; expected FAIL.
- [ ] **Step 3:** Implement; git facts via the `git` executable, never a library; the GitHub adapter only reads the environment.
- [ ] **Step 4:** Run; expected PASS. Commit `feat: идентичность отправителя и адаптеры источников`.

### Task 9: Wire protocol

**Files:**
- Create: `crates/lr-proto/src/{messages.rs,framing.rs,version.rs}`
- Test: `crates/lr-proto/tests/proto.rs`

**Interfaces:**
- Consumes: Task 2 and 7 types.
- Produces: `const PROTOCOL_MAJOR: u16`, `struct Hello {major: u16, minor: u16, role: Role, machine: MachineId}`, `enum Role {Hub, Runner, Client, Session}`, `enum ClientMsg {Submit(SubmitReq), Cancel{target: String, force: bool, reason: Option<String>}, Wait{job: JobId}, Status(StatusReq), Jobs(JobsReq), Heartbeat}`, `enum RunnerMsg {Register(RunnerInfo), Heartbeat, Chunk{job: JobId, seq: u64, bytes: Vec<u8>}, StepResult{job, seq: u64, result: StepResult}, JobResult{job, outcome: JobOutcome}, Acquire{..}, Release{..}}`, `enum HubMsg {Assign(Assignment), CancelJob{job: JobId, grace_s: u32}, Ack{job: JobId, seq: u64}, Refuse{reason: String}, Event(JobEventView)}`, `fn write_frame<W>(&mut W, &impl Serialize)`, `fn read_frame<R, T>(&mut R) -> Result<Option<T>>` (length-prefixed JSON, max frame 4 MiB), `fn check_compat(local: u16, remote: u16) -> Result<(), String>`.

- [ ] **Step 1: Write failing tests:** `roundtrip_every_message_variant`, `oversized_frame_rejected`, `truncated_frame_is_error_not_panic`, `major_mismatch_refused_with_message_naming_both_versions`, `minor_difference_accepted`.
- [ ] **Step 2:** Run `cargo test -p lr-proto`; expected FAIL.
- [ ] **Step 3:** Implement; every job-scoped runner message carries a `seq` so the hub can apply it idempotently.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(proto): протокол, кадры, проверка версий`.

### Task 10: Daemon engine

**Files:**
- Create: `crates/lr-daemon/src/{engine.rs,jobs.rs,reaper.rs,persist.rs,config.rs}`
- Test: `crates/lr-daemon/tests/engine.rs`

**Interfaces:**
- Consumes: Tasks 2–9.
- Produces: `struct DaemonConfig {state_dir, max_timeout_s, grace_s, starvation, retention, pools: Vec<PoolCfg>, workers: BTreeMap<Level,u32>, hub: Option<HubCfg>}`, `struct Engine`, `Engine::start(cfg, store: Store) -> Engine`, `submit(&self, SubmitReq) -> Result<JobId, SubmitError>` (rejects unreachable fits and missing `timeout_s`, caps timeout), `cancel(&self, who: &Identity, target: &str, force: bool, reason: Option<&str>) -> CancelResult`, `subscribe(&self) -> broadcast::Receiver<JobEventView>`, `snapshot(&self) -> Snapshot`, `mark_submitter_dead(&self, JobId)`. Queued jobs are persisted in `hub.db` (`queue_entry`, with `pool`, `runner`, `lease`) and restored on start; on start any job left `running` by a dead daemon is marked `failed` with reason `daemon restarted`.

- [ ] **Step 1: Write failing tests** with a fake runner: `job_goes_queued_running_passed`, `cancel_queued_removes_it_without_starting`, `cancel_other_label_needs_force_and_is_recorded`, `finish_vs_cancel_race_leaves_single_final_state`, `restart_restores_queued_and_reaps_running`, `dead_submitter_drops_queued_and_kills_running_unless_detached`, `timeout_above_cap_is_lowered`, `reject_on_submit_when_never_fits`, `result_delivered_twice_writes_one_history_row`.
- [ ] **Step 2:** Run `cargo test -p lr-daemon --test engine`; expected FAIL.
- [ ] **Step 3:** Implement one async task owning the queue and scheduler state, driven by a command channel; `pick_next` is called on every state change; resources are released only after the process tree is confirmed gone.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(daemon): движок очереди`.

### Task 11: Local IPC server and client

**Files:**
- Create: `crates/lr-daemon/src/ipc.rs`, `crates/lr-cli/src/client.rs`
- Test: `crates/lr-daemon/tests/ipc.rs`

**Interfaces:**
- Consumes: Tasks 9–10.
- Produces: `async fn serve_ipc(engine: Arc<Engine>, endpoint: &IpcEndpoint) -> io::Result<()>` (named pipe `\\.\pipe\al-localrunner-<user>` on Windows, Unix socket under `$XDG_RUNTIME_DIR` or the state directory elsewhere, restricted to the OS user), `async fn connect_local(endpoint: &IpcEndpoint) -> io::Result<Conn>`, `Conn::request(&mut self, ClientMsg) -> impl Stream<Item = HubMsg>`. Submitter liveness: the connection of a non-detached submit is watched; its drop calls `mark_submitter_dead`.

- [ ] **Step 1: Write failing tests:** `submit_over_ipc_returns_job_id`, `wait_streams_events_and_ends_with_result`, `dropping_wait_does_not_cancel`, `dropping_submit_connection_marks_submitter_dead`, `detached_submit_survives_connection_drop`, `socket_is_not_world_accessible` (Unix only, mode `0600`).
- [ ] **Step 2:** Run `cargo test -p lr-daemon --test ipc`; expected FAIL.
- [ ] **Step 3:** Implement with tokio native named pipes and `UnixListener`; no extra IPC crate.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(daemon): локальный IPC`.

### Task 12: CLI commands

**Files:**
- Create: `crates/lr-cli/src/cmd/{submit.rs,jobs.rs,cancel.rs,status.rs,history.rs,log.rs,stats.rs}`, `crates/lr-cli/src/handle.rs`
- Test: `crates/lr-cli/tests/cli.rs`

**Interfaces:**
- Consumes: Task 11 client and Task 8 identity.
- Produces: commands `submit --as L [--note T] [--profile N] [--level X] [--timeout-s N] [--platform a,b] [--detach] [--wait]`, `wait <id>`, `jobs --mine [--active]`, `cancel <id|group|--mine> [--force] [--reason T]`, `status [--json] [--project P] [--source S] [--as L]`, `history [filters] [--json]`, `history show <id>`, `log <id> [--step N] [--tail N]`, `stats [--project P] [--profile N]`; `submit` writes the handle `temp/runner/<job-id>.json`; the log file `temp/runner/<job-id>.log` is written from the stream. Exit codes: 0 passed, 1 failed, 2 timeout, 3 cancelled, 64 usage error. `submit` sends `ClientMsg::Bye` right after `Submitted` in every non-detached case; `wait` and `submit --wait` wait on a second, separate connection; the identity pid recorded by `collect_session` becomes the parent process of the CLI with its start time (change in `lr-core/src/identity.rs`); `jobs --mine` fills `label` and `os_user` in `JobsReq` itself.

- [ ] **Step 1: Write failing tests** against an in-process daemon: `submit_without_as_is_rejected`, `submit_wait_exit_code_matches_result`, `jobs_mine_lists_only_same_label_and_user`, `cancel_mine_cancels_all_active_of_caller`, `status_json_has_project_source_label_fields`, `handle_file_written_and_found_after_restart`, `lowering_timeout_allowed_raising_rejected`, `submit_wait_interrupted_does_not_cancel` (the waiting process is killed; the job stays), `submit_records_parent_pid_not_cli_pid`, `submit_sends_bye_after_submitted`, `status_shows_wait_reason_with_numbers` (a queued job that waits for memory prints the pool, the needed and the free amount, e.g. `waits for memory in pool win-host: needs 6144 MB, free 2048 MB`; a job behind an `exclusive` tag names the holder; `--json` carries the same reason as structured fields).
- [ ] **Step 2:** Run `cargo test -p lr-cli --test cli`; expected FAIL.
- [ ] **Step 3:** Implement with clap derive; text and `--json` render from the same snapshot types.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(cli): команды очереди, истории и логов`.

### Task 13: Daemon lifecycle, config, machine id

**Files:**
- Create: `crates/lr-cli/src/cmd/{daemon.rs,up.rs,down.rs}`, `crates/lr-daemon/src/{state_dir.rs,machine.rs}`
- Test: `crates/lr-cli/tests/lifecycle.rs`

**Interfaces:**
- Produces: `al-localrunner daemon` (foreground), `up [--levels light,heavy,ci] [--workers light=1,heavy=1]`, `down [--drain|--cancel] [--levels ...]`, `fn state_dir() -> PathBuf` (`%LOCALAPPDATA%\Al.LocalRunner`, `~/.local/share/al-localrunner`), `fn machine_id(state_dir: &Path) -> MachineId` (created once, stable), config file `config.toml` in the state directory (pools, workers, caps, retention, `network`, hub address).

- [ ] **Step 1: Write failing tests:** `machine_id_is_stable_across_calls`, `up_starts_daemon_and_second_up_is_idempotent`, `down_drain_waits_for_running_jobs`, `down_cancel_cancels_everything`, `daemon_refuses_config_without_pool_limits_when_hub_enabled`.
- [ ] **Step 2:** Run `cargo test -p lr-cli --test lifecycle`; expected FAIL.
- [ ] **Step 3:** Implement; `up` spawns the daemon detached with a pid file and waits for the endpoint to answer.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(cli): up, down, конфигурация, machine id`.

### Task 13b: Levels defined outside the code

Inserted after Tasks 1-13 were built with a closed `Level` enum; this task removes it. Spec: `docs/architecture.html`, section 4 (levels).

**Files:**
- Create: `crates/lr-core/src/levels.rs` (`LevelDef`, `LevelSet`, `levels.toml` parsing, built-in defaults, `LevelDef::unmet(&Facts) -> Vec<Unmet>`), `crates/lr-daemon/src/probe.rs` (disk free, programs on `PATH`, `check` command with 10 s timeout), `crates/lr-cli/src/cmd/levels.rs`
- Modify: `crates/lr-core/src/{model.rs,profile.rs,queue.rs,scheduler.rs}` (`Level` becomes a validated string newtype, no `Ord`; priority comes from `LevelSet`), `crates/lr-proto/src/*` (wire `Level` as string; `Register` carries the probe result per level; `PROTOCOL_MINOR` bumped), `crates/lr-daemon/src/*` (load `levels.toml` from the state directory, probe at registration and on `up`, offer a runner only the levels it satisfies, anti-starvation uses the highest defined priority), `crates/lr-cli/src/cmd/up.rs` (`up --levels` validates and refuses with the difference), `crates/lr-store` if a column assumes the three names.
- Test: `crates/lr-core/tests/levels.rs`, `crates/lr-daemon/tests/levels.rs`, `crates/lr-cli/tests/levels.rs`

**Interfaces:**
- Produces: `struct Level(String)` with `Level::parse(&str) -> Result<Level, LevelError>` (`[a-z0-9-]`, 1-32 chars); `struct LevelDef {name, description, priority: i32, min_memory_mb: Option<u64>, min_cpu: Option<u32>, min_disk_mb: Option<u64>, programs: Vec<ProgramReq>}`; `struct LevelSet` (`LevelSet::defaults()`, `LevelSet::from_toml(&str)`, `get`, `priority(&Level) -> i32`, `max_priority()`); `struct Facts {pool_memory_mb, pool_cpu, free_disk_mb, programs: BTreeMap<String, bool>}`; `enum Unmet {Memory{needs, has}, Cpu{needs, has}, Disk{needs, has}, ProgramMissing(String), CheckFailed{program, reason}}`; `al-localrunner levels [--json]` lists definitions and, per registered runner, the unmet characteristics.
- Consumes: Tasks 2-4, 9, 10, 13.

- [ ] **Step 1: Write failing tests:** in `lr-core/tests/levels.rs`: `defaults_need_no_file` (`light`, `heavy` priority 0, `ci` priority 100), `custom_label_from_toml`, `level_name_validation` (uppercase, space, empty, 33 chars rejected), `priority_comes_from_definition_not_name` (a level `nightly` with priority 100 goes before `light`; a level named `ci` with priority 0 does not), `unknown_level_in_profile_rejected_with_defined_list`, `generic_core_has_no_level_literals` (no `"light"`, `"heavy"` or `"ci"` string literal in `lr-core/src` or `lr-daemon/src` outside `levels.rs` defaults and tests), `unmet_reports_numbers_for_memory_cpu_disk`, `programs_list_accepts_names_and_name_with_check`; in `lr-daemon/tests/levels.rs`: `runner_below_min_memory_gets_no_such_level_and_others_stay`, `missing_program_makes_level_unavailable_and_reported`, `failing_check_command_is_reported_with_reason`, `check_command_over_10_s_is_failure`, `disk_below_minimum_reported`, `submit_with_level_nobody_can_carry_rejected_with_reason`, `anti_starvation_uses_highest_defined_priority`; in `lr-cli/tests/levels.rs`: `up_levels_refuses_unmet_level_with_difference`, `levels_command_lists_unmet_per_runner`, `old_three_level_setup_works_without_a_file`.
- [ ] **Step 2:** Run `cargo test --workspace`; expected FAIL on the new tests.
- [ ] **Step 3:** Implement the refactor; every existing test keeps passing with the default set; probes of one `up` run in parallel with a total time limit.
- [ ] **Step 4:** Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check`. Commit `refactor(core): уровни задаются снаружи, характеристики уровня`.

### Task 13c: Runner as a separate role

Inserted after Tasks 1-13b: the runner leaves the hub process. Spec: `docs/architecture.html`, section 3 (roles); the broker model there supersedes the hub-only engine of this task.

**Files:**
- Create: `crates/lr-runner/src/{agent.rs,buffer.rs,local.rs}` (agent core over any `Dial`, `Outbox`, local dial through `IpcEndpoint`), `crates/lr-cli/src/cmd/runner_proc.rs` (`al-localrunner runner` foreground runner process)
- Modify: `crates/lr-daemon/src/{ipc_conn.rs,actor.rs,reports.rs,local.rs}` (a session whose `Hello` role is `runner` becomes a `RunnerLink`; `LocalRunner` and every direct call of `run_job` leave `lr-daemon`), `crates/lr-daemon/Cargo.toml` (no dependency on `lr-exec`), `crates/lr-proto/src/messages.rs` (`Register` carries the list of jobs the runner still runs with their last acked seq; `PROTOCOL_MINOR` bumped), `crates/lr-cli/src/cmd/{up.rs,down.rs}` (`up` starts the hub, then a runner process per configured runner; `down` drains or cancels through the runner first, then stops the hub)
- Test: `crates/lr-runner/tests/agent.rs`, `crates/lr-daemon/tests/separation.rs`, `crates/lr-cli/tests/lifecycle.rs`

**Interfaces:**
- Produces: `trait Dial {async fn connect(&self) -> io::Result<Conn>}`, `async fn run_agent(cfg: AgentCfg, dial: impl Dial) -> io::Result<()>` (register, heartbeat every 15 s, run `Assign` through `run_job`, stream chunks with seq numbers through `Outbox`, deliver buffered results after reconnect, kill running jobs after `hub_lost_grace`), `struct LocalDial(IpcEndpoint)`; the hub reconciles `Register.running` with `hub.db`: a job whose runner re-registers keeps running, a job whose runner does not return within `hub_lost_grace` becomes `failed` with reason `runner lost`. The submitter-directory log file of local jobs is written by the runner process.
- Consumes: Tasks 9-13b. Task 15 adds only the TLS dial, the registry and the join flow on top of this agent.

- [ ] **Step 1: Write failing tests:** `crates/lr-daemon/tests/separation.rs`: `hub_has_no_dependency_on_lr_exec` (cargo metadata of `lr-daemon`), `hub_with_no_runner_connected_leaves_job_queued_with_reason`; `crates/lr-runner/tests/agent.rs`: `local_runner_connects_over_ipc_with_runner_hello`, `job_runs_through_runner_process_and_result_reaches_hub`, `hub_restart_keeps_running_job_when_runner_reregisters`, `runner_gone_beyond_grace_fails_its_jobs_with_reason`, `outbox_resends_in_order_and_hub_dedups_by_seq`, `log_file_in_submitter_dir_written_by_runner`; `crates/lr-cli/tests/lifecycle.rs`: `up_starts_hub_and_runner_and_down_stops_both`, `down_drain_waits_for_job_on_runner`, `down_cancel_reaches_runner_and_kills_tree`. Existing engine tests keep a fake runner link; every other existing test keeps passing.
- [ ] **Step 2:** Run `cargo test --workspace`; expected FAIL on the new tests.
- [ ] **Step 3:** Implement the move; Windows kill-on-close now ties job processes to the runner process, not to the hub; the Task 10 rule 'running at restart becomes failed' applies only to jobs whose runner did not re-register.
- [ ] **Step 4:** Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check`. Commit `refactor: раннер — отдельная роль и процесс, hub не исполняет джобы`.

### Task 14: TLS identity, join codes, keys, registry

**Files:**
- Create: `crates/lr-net/src/{cert.rs,join_code.rs,keys.rs,registry.rs}`
- Test: `crates/lr-net/tests/join.rs`

**Interfaces:**
- Produces: `fn ensure_hub_cert(dir: &Path) -> Result<HubCert>` (self-signed, generated once), `HubCert::fingerprint(&self) -> String` (SHA-256 hex), `struct JoinCode(String)`, `Registry::issue_code(&mut self, kind: Kind, now) -> JoinCode` (encodes address and fingerprint; single use, 1 h), `Registry::redeem(&mut self, code: &str, public_key: [u8;32], info: MachineInfo, now) -> Result<MachineName, JoinError>`, `Registry::verify(&self, name: &str, nonce: &[u8], sig: &[u8]) -> bool`, `Registry::remove(&mut self, name: &str)`, `fn client_config(fingerprint: &str) -> rustls::ClientConfig` (accepts only a certificate with that fingerprint), `enum Kind {Runner, Client}`.

- [ ] **Step 1: Write failing tests:** `code_is_single_use`, `code_expires_after_one_hour`, `wrong_code_rejected_without_hint`, `signature_with_registered_key_verifies`, `removed_machine_no_longer_verifies`, `client_rejects_certificate_with_other_fingerprint`, `pool_limits_mismatch_on_join_is_rejected_with_difference` (via `MachineInfo.pool_limits`).
- [ ] **Step 2:** Run `cargo test -p lr-net`; expected FAIL.
- [ ] **Step 3:** Implement with rcgen, rustls, sha2, ed25519-dalek; the registry persists in `hub.db` (tables `machine`, `join_code`, see `docs/architecture.html`), public keys only.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(net): сертификат хаба, коды подключения, ключи`.

### Task 15: Hub listener and runner agent

**Files:**
- Create: `crates/lr-daemon/src/hub.rs`, `crates/lr-runner/src/tls_dial.rs`, `crates/lr-cli/src/cmd/{runner.rs,client.rs}` (the agent core, `Outbox` and local dial already exist from Task 13c)
- Test: `crates/lr-runner/tests/agent.rs`

**Interfaces:**
- Consumes: Tasks 9, 10, 13c, 14.
- Produces: `async fn serve_hub(engine: Arc<Engine>, cfg: &HubCfg, registry: Registry) -> io::Result<()>` (listens only on `cfg.listen`, refuses a wildcard address at startup), `async fn run_agent(cfg: AgentCfg) -> io::Result<()>` (dial, handshake, signed nonce, `Register`, heartbeat every 15 s, run `Assign` through `run_job`, stream `Chunk`s and results), `struct Outbox` (sequence-numbered buffer flushed in order after reconnect), hub marks a runner `offline` after 3 missed heartbeats; an agent whose hub is unreachable for `hub_lost_grace` (default 10 min) kills its running jobs. CLI: `runner add`, `runner join <host:port> <code> --levels --platforms --tags --pool --workers --network`, `runner list`, `runner remove <name>`.

- [ ] **Step 1: Write failing tests** (hub and agent in one process over loopback TLS): `agent_runs_assigned_job_and_hub_records_result`, `chunks_reach_hub_in_order`, `reconnect_resends_outbox_and_hub_dedups_by_seq`, `missed_three_heartbeats_marks_offline`, `hub_lost_beyond_grace_kills_running_job`, `removed_runner_is_refused_at_handshake`, `wildcard_listen_address_rejected_at_start`, `incompatible_major_refused_with_reason`.
- [ ] **Step 2:** Run `cargo test -p lr-runner --test agent`; expected FAIL.
- [ ] **Step 3:** Implement; the engine sees an agent as a `RunnerView` fed by `Register` and heartbeats.
- [ ] **Step 4:** Run; expected PASS. Commit `feat: канал раннеров, агент и подключение`.

### Task 16: Source delivery

**Files:**
- Create: `crates/lr-core/src/delivery.rs`, `crates/lr-runner/src/checkout.rs`
- Test: `crates/lr-core/tests/delivery.rs`, `crates/lr-runner/tests/checkout.rs`

**Interfaces:**
- Produces: `fn check_pushed(repo: &Path) -> Result<PushedCommit, DeliveryError>` (`HEAD` reachable from a remote branch, else an error whose text says to commit and push, no merge request needed), `fn dirty_files(repo: &Path) -> Vec<String>`, `fn profiles_at_commit(repo: &Path, commit: &str) -> Result<String>` (`git show <commit>:.localrunner/profiles.toml`), `struct PushedCommit {origin, commit, branch}`, `async fn checkout(state_dir: &Path, origin: &str, commit: &str) -> Result<PathBuf>` (one clone per project under the runner state directory, `git fetch`, detached clean checkout, uses the runner machine's own git credentials).

- [ ] **Step 1: Write failing tests** with temporary git repos: `unpushed_head_rejected_with_push_hint`, `dirty_tree_warns_but_passes`, `profile_read_from_commit_not_working_tree`, `checkout_is_detached_and_clean_after_previous_job_left_files`, `checkout_path_with_spaces_and_cyrillic_works`, `two_projects_get_separate_checkouts`.
- [ ] **Step 2:** Run `cargo test -p lr-core --test delivery -p lr-runner --test checkout`; expected FAIL.
- [ ] **Step 3:** Implement through the `git` executable; the core stores no credentials.
- [ ] **Step 4:** Run; expected PASS. Commit `feat: доставка запушенного коммита на раннер`.

### Task 17: Remote routing, groups, platforms

**Files:**
- Modify: `crates/lr-daemon/src/{engine.rs,jobs.rs}`
- Create: `crates/lr-daemon/src/groups.rs`
- Test: `crates/lr-daemon/tests/routing.rs`

**Interfaces:**
- Consumes: Tasks 4, 10, 15, 16.
- Produces: `submit` with `platforms` creates one job per platform under one `GroupId`; `GroupState` derived (`running` until every job is final, `passed` only if all passed); `cancel` by group id cancels all active jobs under the ownership rule; a job without `platforms` goes only to runners with the submitter `MachineId`; `network` label defaults from the peer address (private subnet label, public address gets its own) and is overridable by config.

- [ ] **Step 1: Write failing tests:** `five_platforms_make_five_jobs_one_group`, `failed_job_does_not_cancel_siblings`, `group_stays_running_while_one_job_waits_for_offline_runner`, `group_passed_only_if_all_passed`, `cancel_group_cancels_active_jobs_only`, `no_platforms_stays_on_submitter_machine`, `same_runner_is_lan_for_one_submitter_and_remote_for_another`, `vpn_override_puts_peer_in_same_network`.
- [ ] **Step 2:** Run `cargo test -p lr-daemon --test routing`; expected FAIL.
- [ ] **Step 3:** Implement on top of `pick_next`; the engine builds `RunnerView`s and per-submitter `QueueEntry` fields from the registry and the connection.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(daemon): маршрутизация по платформам и группы`.

### Task 18: Client role

**Files:**
- Create: `crates/lr-cli/src/net_client.rs`
- Modify: `crates/lr-cli/src/{client.rs,cmd/submit.rs}`
- Test: `crates/lr-cli/tests/net_client.rs`

**Interfaces:**
- Consumes: Tasks 12, 14, 15.
- Produces: `async fn connect_hub(cfg: &ClientCfg) -> io::Result<Conn>` returning the same `Conn` as the local connection, so every CLI command works against a remote hub through `--hub <name>` or `config.toml`; CLI `client add`, `client join <host:port> <code>`; submits carry the client `MachineId`; dead-submitter detection uses the lifetime of this connection.

- [ ] **Step 1: Write failing tests:** `client_join_then_submit_runs_on_runner`, `two_clients_share_one_queue_and_see_each_other`, `cancel_from_other_machine_needs_force`, `client_connection_drop_kills_running_job_unless_detached`, `revoked_client_refused`.
- [ ] **Step 2:** Run `cargo test -p lr-cli --test net_client`; expected FAIL.
- [ ] **Step 3:** Implement by reusing the Task 9 messages over the Task 14 channel.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(cli): роль клиента с нескольких машин`.

### Task 19: `ci` runner acquire over the channel

**Files:**
- Create: `crates/lr-cli/src/cmd/ci.rs`, `crates/lr-daemon/src/ci.rs`
- Test: `crates/lr-daemon/tests/ci.rs`

**Interfaces:**
- Consumes: Tasks 3, 4, 15.
- Produces: `al-localrunner ci acquire --tags a,b [--exclusive x] --mem-mb N --cpu N` (blocks until granted, prints a lease id, then the GitHub job runs) and `ci release <lease>`; the `github` source adapter fills the job record; a lease is released when the runner process or connection dies; `ci` leases are queued at `ci` priority and count in the pool of the machine the runner lives on, locally or through the runner channel.

- [ ] **Step 1: Write failing tests:** `ci_acquire_waits_for_conflicting_heavy_to_finish_and_does_not_preempt_it`, `ci_acquire_goes_before_waiting_light`, `lease_released_when_holder_connection_drops`, `remote_ci_runner_acquire_counts_in_its_pool`, `non_al_ci_jobs_never_touch_the_queue`.
- [ ] **Step 2:** Run `cargo test -p lr-daemon --test ci`; expected FAIL.
- [ ] **Step 3:** Implement a lease as a queue entry of kind `Lease`; no preemption. Add `al-localrunner install ci --repo <owner/repo>` (label `al-ci`, registration token read from the prompt or an environment variable and never written to disk; idempotent) and supervision of the runner in the user session, never as a system service: user-session process on Windows, systemd user unit on Linux, LaunchAgent on macOS; test the idempotence and that no token appears in any file under the state directory.
- [ ] **Step 4:** Run; expected PASS. Commit `feat: захват ресурсов ci-раннером`.

### Task 20: DataSource and loopback HTTP server

**Files:**
- Create: `crates/lr-dashboard/src/{source.rs,server.rs,security.rs,sse.rs}`
- Test: `crates/lr-dashboard/tests/security.rs`

**Interfaces:**
- Consumes: Tasks 7, 10.
- Produces: `trait DataSource: Send + Sync {fn snapshot(&self) -> Snapshot; fn history(&self, &HistoryFilter) -> Vec<JobRecord>; fn log(&self, &JobId, Option<&str>, Option<usize>) -> Vec<u8>; fn stats(&self, ..) -> Vec<ProfileStats>; fn subscribe(&self) -> broadcast::Receiver<JobEventView>}`, `struct LocalSource` (hub live state plus the local store), `async fn serve_dashboard(src: Arc<dyn DataSource>, actions: Arc<dyn Actions>) -> io::Result<DashboardHandle>` (random port on `127.0.0.1`/`::1`, per-start token exchanged for an `HttpOnly; SameSite=Strict` cookie and removed from the URL), `trait Actions {fn cancel(&self, ..)}`.

- [ ] **Step 1: Write failing tests:** `binds_only_loopback_and_aborts_if_not`, `request_without_cookie_refused`, `foreign_host_header_refused_dns_rebinding`, `foreign_origin_refused`, `post_without_anti_forgery_header_refused`, `sse_requires_cookie_and_reconnects_with_last_event_id`, `csp_header_forbids_inline_script_eval_and_external_hosts`, `log_text_is_served_as_text_plain_not_markup`, `another_data_source_plugs_in_without_touching_the_server` (a fake `DataSource` implemented in the test is served by `serve_dashboard` and its snapshot, history and events reach the HTTP and SSE responses). `DataSource` carries no machine or runner address: a later hub-backed source (`docs/architecture.html`, section 9.3) implements the same trait; only `LocalSource` is built now.
- [ ] **Step 2:** Run `cargo test -p lr-dashboard --test security`; expected FAIL.
- [ ] **Step 3:** Implement with axum on a manually bound listener; after binding, read back the local address and exit with an error if it is not loopback.
- [ ] **Step 4:** Run; expected PASS. Commit `feat(dashboard): источники данных и защищённый сервер`.

### Task 21: Dashboard page — Queue, History, live log, cancel

**Files:**
- Create: `crates/lr-dashboard/assets/{index.html,app.js,style.css}`, `crates/lr-dashboard/src/assets.rs`
- Test: `crates/lr-dashboard/tests/page.rs`

**Interfaces:**
- Produces: one self-contained page embedded with `include_str!` (no external scripts, fonts or requests); tabs Queue and History; job card with steps timeline, live log (follow, pause, search, copy, download), cancel with a confirmation naming the job (recorded as `dashboard:<os user>` with the typed reason); light and dark theme from the system; works at phone width; visible keyboard focus; text rendered via `textContent` only; state in the URL hash.

- [ ] **Step 1: Write failing tests** (Rust, parsing the embedded HTML): `page_has_no_external_urls`, `page_has_no_inline_event_handlers_or_eval`; plus browser checks run through the playwright MCP in Step 4.
- [ ] **Step 2:** Run `cargo test -p lr-dashboard --test page`; expected FAIL.
- [ ] **Step 3:** Implement the page in plain JavaScript; the snapshot mode reads the embedded JSON and disables live updates and actions.
- [ ] **Step 4:** Run the tests, then start `al-localrunner dashboard`, open it with playwright, and verify: a queued job appears, expanding shows the timeline, the live log follows, cancel needs confirmation, a hash URL restores the filter, a waiting job shows its reason with the same numbers as `status` (pool, needed, free, holder), and no console errors. Commit `feat(dashboard): страница очереди, истории и живого лога`.

### Task 22: Dashboard — Stats, Runners, filters, export, snapshot

**Files:**
- Modify: `crates/lr-dashboard/assets/{index.html,app.js,style.css}`
- Create: `crates/lr-cli/src/cmd/dashboard.rs`
- Modify: `crates/lr-store/src/{schema.rs,stats.rs,record.rs}` (migration adding `waited_on` to `job`), `crates/lr-daemon/src/{commands.rs,actor.rs}` (write it when the job starts)
- Test: `crates/lr-cli/tests/snapshot.rs`, `crates/lr-store/tests/saturation.rs`

**Interfaces:**
- Produces: tabs Stats (inline SVG charts of pass rate, wait and duration median and p95, per-level day lanes, and **saturation**: the share of jobs and of total waiting time per cause of the last wait: `worker`, `memory`, `cpu`, `exclusive tag`, `no runner`, so the owner sees which limit to raise) and Runners (per machine and pool: online, last seen, platform, levels, the unmet characteristics of a level the machine does not carry with the numbers, gauges, held tags); the job row stores `waited_on` (the last wait reason kind, `none` if it started at once) and `Store::saturation(filter)` returns counts and wait sums per kind; filters, `?as=<label>` pinning, CSV and JSON export, shortcuts (`/`, `j`, `k`, `Enter`, `Esc`, `g q|h|s|r`, `?`); `al-localrunner dashboard` (live) and `status --html [path]` (default `temp/runner/queue.html`, logs only with `--with-logs`).

- [ ] **Step 1: Write failing tests:** `snapshot_file_is_self_contained_and_opens_without_listener`, `snapshot_omits_logs_unless_with_logs`, `dashboard_command_prints_url_with_token_and_stops_on_ctrl_c`; in `lr-store/tests/saturation.rs`: `waited_on_is_stored_and_migrated_from_older_db`, `saturation_splits_wait_by_cause`, `job_started_at_once_counts_as_none`.
- [ ] **Step 2:** Run `cargo test -p lr-cli --test snapshot`; expected FAIL.
- [ ] **Step 3:** Implement the tabs and commands.
- [ ] **Step 4:** Run the tests, then verify in the browser through playwright: charts render, filters and export work, shortcuts switch tabs, phone width (375 px) has no horizontal scroll. Commit `feat(dashboard): статистика, раннеры, снимок`.

### Task 23: Plugin skills, OS notes, README

**Files:**
- Modify: `plugin/skills/{install-runner,up-runner,down-runner,status-runner,run}/SKILL.md`, `plugin/shared/os/{windows,linux,macos}.md`, `README.md`

**Interfaces:**
- Consumes: the CLI contract of Tasks 12, 13, 15, 18, 19, 22.

- [ ] **Step 1:** Rewrite each skill so it calls the real commands with the inputs of the skills table in `docs/architecture.html` (section 11); `run` documents `--as`, `--platform`, remote jobs and the push requirement; `install-runner` documents `runner join` and `client join`; skills stay in English.
- [ ] **Step 2:** Fill the OS files with the per-OS differences of the OS table in `docs/architecture.html` (section 10: IPC endpoint, tree kill, supervision, emulator acceleration, sleep prevention) and the emulator or simulator notes for Android and iOS runners.
- [ ] **Step 3:** Update the README status from skeleton to the implemented commands.
- [ ] **Step 4:** Run `claude plugin validate plugin` if available; expected no errors. Commit `docs: скилы, заметки по ОС, README`.

### Task 24: End-to-end and final verification

**Files:**
- Create: `crates/lr-cli/tests/e2e.rs`

- [ ] **Step 1: Write the end-to-end test:** start a hub with two runner agents with different `network` labels and platforms, a client from a third process, a five-platform profile, and assert: five jobs run in parallel within pool limits, a `ci` lease overtakes waiting jobs without stopping a running one, a killed agent mid-job is reaped after the grace period (shortened for the test), and history on each runner machine holds only its own jobs.
- [ ] **Step 2:** Run `cargo test --workspace`; expected all PASS.
- [ ] **Step 3:** Run `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check licenses`; expected clean.
- [ ] **Step 4:** Check every file with `wc -l` for the 500-line limit and grep the sources for comments outside doc comments; fix any finding.
- [ ] **Step 5:** Run the real flow on Windows with a Docker runner in the same pool and a Linux runner in the local network, record the result in the issue #1, and commit `test: сквозная проверка нескольких машин`.
