# Al.LocalRunner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the system described in `docs/architecture.html`: queued runners over several machines, one Rust binary `localrunner` with three roles (hub, broker, runner), dashboard, history.

**Architecture:** A Cargo workspace in five layers (spec 13.1). `lr-core` holds pure logic. `lr-abstractions` holds the I/O contracts (`Clock`, `Store`, `Transport`, `Listener`, `ProcessRunner`, `SecretSource`, `LogSink`, `DataSource`) plus in-memory fakes under feature `test-support`; `lr-proto` holds wire types. Implementations (`lr-net`, `lr-store`, `lr-exec`, `lr-obs`) depend only on core and abstractions. Roles (`lr-hub`, `lr-broker`, `lr-runner`, `lr-dashboard`) receive implementations through their constructors and never depend on an implementation crate; only `lr-cli` wires them. Every role is built and tested first against the fakes, real implementations come after.

**Tech Stack:** Rust stable, tokio, serde, serde_json, toml, clap, rusqlite (bundled), rustls + tokio-rustls + rcgen, ed25519-dalek, sha2, axum (dashboard), windows-sys (Job Object), libc.

**Spec:** `docs/architecture.html` (the only design document; sections are cited as "spec N.M"). This is a rewrite: the earlier implementation is archived as tag `archive/impl-v1` (worktree `.claude/worktrees/impl`) and is a quarry, not a base.

## Global Constraints

- Work only in branch `feat/localrunner-v2` (worktree `.claude/worktrees/impl2`); never touch `archive/impl-v1` or `main` from tasks.
- Binary name `localrunner`; workspace crates under `crates/` named as in spec 13.1 plus `lr-obs`.
- Dependency rule (enforced by a test in Task 1): roles depend only on `lr-core`, `lr-abstractions`, `lr-proto`; implementations only on `lr-core`, `lr-abstractions` (`lr-net` also `lr-proto`); `lr-core` on nothing in the workspace; only `lr-cli` depends on implementations.
- Third-party crates only with a free commercial-use license (MIT, Apache-2.0, BSD, ISC, MPL-2.0 unmodified); enforced by `cargo deny`.
- No code comments except doc comments on public items; every file under 500 lines.
- Generic core: broker, scheduler and queue never name or branch on a consumer, language, toolchain, test framework or platform; platform names and tags are opaque labels compared for equality.
- `timeout_s` is required on every profile and step; no default, no unbounded run.
- Timeout kills the process tree immediately with no soft signal; cancel sends soft signal (Ctrl+Break on Windows, SIGTERM elsewhere), then kills after 10 s (spec 7.3). Broker `lost` deadline = `Started` time (broker clock) + `timeout_s` + `[grace] lost_min` (default 3).
- Protocol frames are JSON up to 4 MiB; duplicates are dropped by (client key, JobId, seq) (spec 3.9, 14).
- One platform per command; no run groups. `--detach` disables the reaper for the whole order.
- Admin commands are never accepted over the network; local IPC clients map to `[[local_user]]` records and zones local/lan/remote by source address (spec 3.8, 3.12).
- Time is read only through `Clock`; storage only through `Store`; processes only through `ProcessRunner` (outside `lr-exec`, `lr-store`, `lr-net`, `lr-obs`, `lr-cli`).
- **Every requirement is covered by a test.** Each normative statement of the spec sections a task cites (a "must", a default value, a state transition, a rejection, a reason code, a config key) needs at least one test that fails if the behavior is removed. The tests listed in a task are a minimum, not the full set: before implementing, the implementer walks the cited spec sections and adds a test for every requirement not yet listed, named after the behavior. A requirement that cannot be tested at the task's level is deferred in writing to the task that owns it (Task 19 at the latest); an untestable requirement is reported to the user, never skipped silently.
- Temporary files go to `temp/` (gitignored); never read or print files under `secrets/`.
- Commits in Russian with a conventional prefix; no `Co-Authored-By`, no agent attribution; plugins at project scope only.

## Review Focus

- Broker restart with queued and running orders, and a runner or hub that vanishes mid-job: nothing runs twice, `lost` fires only after `Started + timeout_s + lost_min` on the broker clock (Tasks 11, 12).
- Message or result delivered twice after reconnect, cancel racing with finish: one final state, one history row (Tasks 11, 13, 14).
- Output that is not UTF-8 or a 10 MB line without newline: log and stream stay bounded (Task 6).
- A command whose grandchildren detach or ignore the stop signal: whole tree gone before resources are released (Task 6).
- "Same machine" order when its runner is offline, busy, or disappears before start: wait only if online-but-busy, otherwise reject and hub does not look for a replacement (Tasks 12, 14).
- Windows paths with spaces and non-ASCII characters, `origin` in https, ssh:// and scp forms with and without `.git`: project name and checkout path correct (Tasks 3, 13).

---

### Task 1: Workspace, license gate, dependency-rule test

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `deny.toml`, `crates/lr-{core,abstractions,proto,net,store,exec,obs,broker,hub,runner,dashboard,cli}/` (`Cargo.toml`, `src/lib.rs`; `lr-cli` also `src/main.rs`)
- Modify: `.gitignore` (add `target/`)
- Test: `crates/lr-cli/tests/layers.rs`

**Interfaces:**
- Produces: the workspace; `lr-abstractions` feature `test-support` (empty for now); binary `localrunner --version`.

- [ ] **Step 1:** Write failing test `layers.rs::dependency_rule_holds`: runs `cargo metadata`, and asserts the allowed-dependency table from Global Constraints for every workspace crate.
- [ ] **Step 2:** Create the workspace and empty crates so the test passes; `main` prints the version through clap.
- [ ] **Step 3:** `deny.toml` allows only the licenses above plus Unicode-3.0, Unlicense, CC0-1.0 and the SQLite public-domain exception.
- [ ] **Step 4:** Run `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo deny check licenses && cargo test -p lr-cli --test layers`. Expected: pass.
- [ ] **Step 4a:** Add `crates/lr-cli/tests/spec_coverage.rs::every_scenario_of_section_16_is_named_by_an_e2e_test` as an ignored placeholder listing the 25 scenario ids; Task 19 un-ignores it.
- [ ] **Step 5:** Commit `chore: каркас workspace по слоям и проверка правила зависимостей`.

### Task 2: Core model, profiles, requirements

**Files:**
- Create: `crates/lr-core/src/{model.rs,profile.rs,requirements.rs,error.rs}`
- Test: `crates/lr-core/tests/{profile.rs,requirements.rs}`

**Interfaces:**
- Produces: `JobId`, `Zone {Local, Lan, Remote}` (ordered), `JobState {Queued, Running, Cancelling, Passed, Failed, Timeout, Cancelled, Lost}`, `StepState`, `StepSpec`, `Profile`, `fn parse_profiles(&str) -> Result<Vec<Profile>, ProfileError>`, `fn resolve_for_platform(&Profile, &str) -> Result<Profile, ProfileError>`; `Requirements {platform, mem_mb, cpu, disk_mb, programs, tags, exclusive, same_machine: Option<MachineRef>, services, secrets, cache}` derived from a profile (max over steps for memory/CPU, union for tags and exclusive); `Capabilities` of a runner and `fn satisfies(&Capabilities, &Requirements) -> Result<(), Mismatch>` (spec 4, 3.10).

- [ ] **Step 1:** Failing tests: `command_and_steps_together_rejected`, `missing_timeout_rejected` (profile and step), `single_command_becomes_one_step`, `platform_override_replaces_command`, `step_timeout_inside_job_timeout`, `requirements_take_max_memory_and_union_tags`, `runner_with_lower_timeout_ceiling_does_not_satisfy`, `missing_program_reports_which`, and a parse of both example profiles from spec 5 asserting `mem_mb == 12288` for `android-tier`.
- [ ] **Step 2:** Run `cargo test -p lr-core`; expected FAIL.
- [ ] **Step 3:** Implement with serde + toml; errors carry profile name and field. Port ideas from `archive/impl-v1` `lr-core/src/{profile,model}.rs` only where they match the spec (drop levels).
- [ ] **Step 4:** Run; expected PASS. Commit `feat(core): модель, профили и требования`.

### Task 3: Core identity, zones, source adapters

**Files:**
- Create: `crates/lr-core/src/{identity.rs,identity_facts.rs,source_github.rs,zone.rs}` (start from the archive files of the same names, adapt to spec 7.1-7.2)
- Test: `crates/lr-core/tests/{identity.rs,zone.rs}` (port archive tests)

**Interfaces:**
- Produces: `Identity {project, source, label, note, os_user, host, sender_pid, sender_start, cwd, branch, repo_root}`, `fn normalize_origin(&str) -> String`, `enum Source {Session, Github{..}}`, `fn zone_of(addr: Option<IpAddr>, broker_addr: &[IpAddr], networks: &[IpNet]) -> Zone`, `fn may_cancel(&Identity, &Identity, force: bool) -> bool`.

- [ ] **Step 1:** Failing tests: `origin_forms_normalize_equally` (https, ssh://, scp, with/without `.git`), `non_git_dir_uses_dir_name`, `github_env_sets_default_label`, `foreign_label_cancel_needs_force`, `other_os_user_ip_counts_as_broker_host_zone` (spec 3.12), `address_in_networks_is_lan_else_remote`.
- [ ] **Step 2:** Run; FAIL. **Step 3:** Port and adapt. **Step 4:** Run; PASS. Commit `feat(core): идентичность отправителя и зоны`.

### Task 4: Core scheduling: pool budget, hole filling, Policy

**Files:**
- Create: `crates/lr-core/src/{scheduler.rs,predict.rs,policy.rs,strategy.rs}`
- Test: `crates/lr-core/tests/{scheduler.rs,predict.rs,policy.rs}`

**Interfaces:**
- Consumes: `Requirements`, `Capabilities`, `Zone` (Tasks 2-3).
- Produces: `Scheduler::new(PoolLimits)`, `try_grant(&JobId, &Requirements) -> Result<(), Wait>`, `release`, `can_ever_fit -> Result<(), Reject>`; `trait Policy { fn order(&self, queue: &[QueueEntry], now: Instant) -> Vec<usize>; fn admit(&self, &Order, &ClientView) -> Result<(), Reject>; }`; strategies `strict_priority`, `fifo`, aging (`age_after_min`, default 30); `fn pick_next(queue, runners, running, now) -> Vec<Assignment>`: head order reserves its earliest start computed from running jobs' `start + timeout_s`; later orders that fit and finish before that start are granted (hole filling, spec 6).

- [ ] **Step 1:** Failing tests: `grant_release_restores_free`, `exclusive_tag_blocks_and_names_holder`, `larger_than_pool_never_fits`, `high_priority_never_preempts_running`, `short_job_fills_hole_before_head_start`, `long_job_does_not_delay_head`, `aging_promotes_after_threshold_and_survives_rebroker`, `fifo_within_priority`, `same_machine_order_only_matches_that_installation`.
- [ ] **Step 2:** Run; FAIL. **Step 3:** Implement pure functions, no clock reads (time is a parameter). **Step 4:** Run; PASS. Commit `feat(core): планировщик, заполнение дыр, политика очереди`.

### Task 5: lr-abstractions: contracts and fakes

**Files:**
- Create: `crates/lr-abstractions/src/{clock.rs,store.rs,transport.rs,process.rs,secrets.rs,logsink.rs,datasource.rs}`, `src/fakes/{mod.rs,clock.rs,store.rs,transport.rs,process.rs,...}` behind feature `test-support`
- Test: `crates/lr-abstractions/tests/fakes.rs`

**Interfaces:**
- Produces (async traits, object-safe or generic as the implementer finds simpler, one choice for the whole crate): `Clock {now() -> Instant, sleep(Duration)}`; `Store` family: `BrokerStore`, `HubStore`, `RunnerStore` with the operations the ER in spec 14 requires (orders, jobs, machines, join codes, outbox with `acked_at`, service registry, cache index, prune, stats); `Transport`/`Listener` (framed JSON messages both ways, peer identity and source address exposed); `ProcessRunner::run_job(JobSpec, LogSink) -> JobOutcome` and `kill_tree`; `SecretSource::get(name)`; `LogSink::write(Record)`; `DataSource` (snapshot + event stream for dashboard). Fakes: `ManualClock` (advance moves sleepers), in-memory stores, `channel_pair()` transports with injectable drop/duplicate, `FakeProcessRunner` (scripted outcomes and durations on the clock).

- [ ] **Step 1:** Failing tests: `manual_clock_wakes_sleepers_on_advance`, `memory_store_outbox_ack_removes_entry`, `channel_pair_can_duplicate_and_drop_frames`, `fake_process_runner_finishes_after_scripted_time`.
- [ ] **Step 2:** Run `cargo test -p lr-abstractions --features test-support`; FAIL. **Step 3:** Implement. **Step 4:** PASS. Commit `feat(abstractions): контракты ввода-вывода и заглушки`.

### Task 6: lr-exec: ProcessRunner (port from archive)

**Files:**
- Create: `crates/lr-exec/src/{guard.rs,job.rs,log.rs,spawn.rs,tree_unix.rs,tree_windows.rs,lib.rs}` copied from `archive/impl-v1`, then adapted
- Test: copy `tests/{backpressure.rs,job.rs,log_writer.rs,tree.rs}` and adapt

**Interfaces:**
- Consumes: `ProcessRunner`, `LogSink`, `Clock` (Task 5), `StepSpec`, `JobOutcome` (Task 2).
- Produces: `ExecRunner: ProcessRunner`; log format with a header line per step (project, source, label, note, branch, cwd, cancel reason).

- [ ] **Step 1:** Copy tests; add failing `timeout_kills_tree_without_soft_signal`, `cancel_sends_soft_signal_then_kills_after_grace_10s`, `step_deadline_is_min_of_step_and_remaining_job`, `failed_step_skips_rest_unless_continue_on_failure`, `non_utf8_and_10mb_line_stay_bounded`.
- [ ] **Step 2:** Run `cargo test -p lr-exec`; FAIL where behavior changed (spec 7.3 timeout rule differs from the archive). **Step 3:** Copy sources, adapt to traits, change timeout behavior. **Step 4:** PASS on Windows and Linux CI-compatible code paths (`cfg`). Commit `feat(exec): запуск процессов и убийство дерева`.

### Task 7: lr-proto: messages and frames

**Files:**
- Create: `crates/lr-proto/src/{frame.rs,runner.rs,api.rs,version.rs}`
- Test: `crates/lr-proto/tests/proto.rs`

**Interfaces:**
- Produces: serde messages of protocol 3.9: `Hello/Join`, `Capabilities` query/reply, `Order`, `Accepted`, `Rejected{code}`, `Started`, `Assign`, `Cancel`, `Chunk{seq}`, `Result`, `Heartbeat`, `Resume`, `Drain`, `Bye`, read requests, services report and stop command; reason codes of spec 15 as `enum ReasonCode`; `fn write_frame/read_frame` (4 MiB cap), `fn negotiate(version)`.

- [ ] **Step 1:** Failing tests: `every_message_round_trips`, `frame_over_4mib_rejected_both_ways`, `unknown_version_refused`, `reason_codes_match_spec_15_list`.
- [ ] **Step 2-4:** Implement, run, commit `feat(proto): сообщения протокола и кадры`.

### Task 8: lr-store: SQLite stores

**Files:**
- Create: `crates/lr-store/src/{broker.rs,hub.rs,runner.rs,schema.rs,prune.rs}`
- Test: `crates/lr-store/tests/{broker.rs,hub.rs,runner.rs,prune.rs}`

**Interfaces:**
- Consumes: store traits (Task 5). Produces: `SqliteBrokerStore`, `SqliteHubStore`, `SqliteRunnerStore` with tables exactly as the spec 14 ER (`broker.db`, `hub.db`, `runner.db`).

- [ ] **Step 1:** Run the contract tests of Task 5 against these (shared test suite exposed from `lr-abstractions` under `test-support`); add failing `final_state_written_once`, `prune_by_age_and_size_skips_active_jobs`, `order_row_removed_after_grant`, `survives_reopen`.
- [ ] **Step 2-4:** Implement with rusqlite, run, commit `feat(store): SQLite-хранилища брокера, hub и раннера`.

### Task 9: lr-net: TLS, codes, keys, listeners

**Files:**
- Create: `crates/lr-net/src/{tls.rs,codes.rs,keys.rs,registry.rs,listener.rs,ipc.rs}`
- Test: `crates/lr-net/tests/{join.rs,keys.rs,ipc.rs}`

**Interfaces:**
- Produces: `TlsListener`, `TlsTransport`, `IpcListener` (named pipe on Windows, Unix socket elsewhere) implementing `Listener`/`Transport`; one-time code with fingerprint inside, TTL from `[join] code_ttl_min`; ed25519 client keys with nonce signing; machine registry (revoke, label).

- [ ] **Step 1:** Failing tests: `code_is_single_use_and_expires`, `wrong_fingerprint_refused`, `signed_nonce_authenticates_key`, `revoked_key_cannot_connect`, `ipc_peer_mapped_to_local_user`, `foreign_os_user_gets_broker_host_zone`.
- [ ] **Step 2-4:** Implement, run, commit `feat(net): TLS, коды вступления, ключи, IPC`.

### Task 10: lr-broker: policy loader

**Files:**
- Create: `crates/lr-broker/src/policy/{file.rs,rules.rs,validate.rs,reload.rs}`
- Test: `crates/lr-broker/tests/policy.rs`

**Interfaces:**
- Consumes: `Policy`, strategies (Task 4). Produces: `PolicyFile` covering every table in the spec 3.5 example (`[queue] [[rule]] [clients] [join] [orphans] [grace] [results] [listen] [networks] [[local_user]] [cancel] [[secrets.allow]] [pool] [limits] [routing] [history] [saturation] [cache] [[services.allow]] [visibility] [commands]`), `fn load(&str) -> Result<PolicyFile, PolicyError>`, `fn reload(&mut self, new) -> Result<Applied, PolicyError>` (`[listen]` only at start; invalid file keeps the old one), `fn into_policy(&PolicyFile) -> Box<dyn Policy>`.

- [ ] **Step 1:** Failing tests: `spec_example_policy_parses` (the full example from the spec), `defaults_applied_grace_3_aging_30`, `listen_change_on_reload_reported_not_applied`, `invalid_reload_keeps_previous`, `rule_without_name_accepted`, `container_listen_value_accepted`.
- [ ] **Step 2-4:** Implement, run, commit `feat(broker): загрузчик политики`.

### Task 11: lr-broker: engine (orders, queue, assignment)

**Files:**
- Create: `crates/lr-broker/src/engine/{mod.rs,orders.rs,assign.rs,runners.rs,events.rs}`, `src/api.rs`
- Test: `crates/lr-broker/tests/{orders.rs,assign.rs,idempotency.rs}`

**Interfaces:**
- Consumes: Tasks 2-5, 7, 10. Produces: `Broker::new(BrokerDeps {clock, store, listener, secrets, policy}) -> Broker`, `Broker::run()`; single owner of state fed by a command channel; events on a broadcast bus for read API. Order lifecycle: `Order → Accepted | Rejected(code)`; assignment to a matching runner; `Started` recorded with broker time; `Result` writes final state once.

- [ ] **Step 1:** Failing tests on fakes + `ManualClock`: `order_accepted_then_assigned_to_matching_runner`, `exceeding_pool_rejected_at_intake`, `duplicate_order_same_key_jobid_seq_is_noop`, `result_twice_one_history_row`, `cancel_of_queued_removes_immediately`, `cancel_other_clients_order_refused`, `hole_filling_respects_head_reservation`, `read_api_hides_others_orders_per_visibility`.
- [ ] **Step 2-4:** Implement, run, commit `feat(broker): движок заказов и выдача раннерам`.

### Task 12: lr-broker: recovery, cancel, same machine, secrets, services

**Files:**
- Create: `crates/lr-broker/src/engine/{recovery.rs,cancel.rs,same_machine.rs,secrets.rs,services.rs,drain.rs}`
- Test: `crates/lr-broker/tests/{recovery.rs,cancel.rs,same_machine.rs,secrets.rs,services.rs}`

**Interfaces:**
- Produces: lost detection (`Started + timeout_s + lost_min`, runner may only shorten), hub-lost and orphan rules (`[orphans]`, `hub_absent`, `sender_gone`), `drain`, same-machine order handling, `secret_missing` and `[[secrets.allow]]` check, services report and stop command with `[[services.allow]]`.

- [ ] **Step 1:** Failing tests: `lost_fires_exactly_at_started_plus_timeout_plus_grace`, `runner_report_before_deadline_cancels_lost`, `restart_requeues_unstarted_and_marks_started_unknown_as_lost_by_deadline`, `same_machine_waits_only_if_runner_online_but_busy`, `same_machine_rejected_if_runner_offline_or_gone_before_start`, `drain_finishes_running_and_refuses_new`, `secret_not_allowed_for_label_rejected`, `cancel_by_wrong_key_refused`.
- [ ] **Step 2-4:** Implement, run, commit `feat(broker): восстановление, отмена, та же машина, секреты, сервисы`.

### Task 13: lr-runner: agent

**Files:**
- Create: `crates/lr-runner/src/{agent.rs,connect.rs,outbox.rs,source.rs,cache.rs,services.rs,settings.rs}`
- Test: `crates/lr-runner/tests/{agent.rs,outbox.rs,source.rs}`

**Interfaces:**
- Consumes: `Transport`, `ProcessRunner`, `RunnerStore`, `Clock`, `SecretSource`. Produces: `RunnerAgent::new(RunnerDeps)`; one job at a time; heartbeat; reconnect with `resume` and outbox replay (seq per job and direction); `source` step: local `cd`; remote mirror in runner state dir (`git fetch`, `git worktree add`, `mkdir` + `clone --mirror` when absent), replaceable by the profile; timeout ceiling and cache settings (spec 3.10); service registry for orphan search by pid + start time.

- [ ] **Step 1:** Failing tests (fakes): `job_runs_and_result_reaches_broker_once_after_reconnect`, `outbox_replay_after_drop_has_no_duplicates_with_dedup`, `timeout_reports_timeout_immediately`, `local_source_step_is_cd`, `remote_source_creates_mirror_when_absent`, `profile_source_overrides_default`, `runner_with_lower_ceiling_advertises_it`, `runner_restart_reports_runner_restarted`.
- [ ] **Step 2-4:** Implement, run, commit `feat(runner): агент раннера`.

### Task 14: lr-hub: client side

**Files:**
- Create: `crates/lr-hub/src/{ipc_server.rs,orders.rs,client.rs,select.rs,outbox.rs,reaper.rs,handles.rs,policy.rs,logs.rs}`
- Test: `crates/lr-hub/tests/{submit.rs,select.rs,outbox.rs,reaper.rs}`

**Interfaces:**
- Consumes: `Transport`, `Listener`, `HubStore`, `Clock`, `ProcessInspector` (new small trait in `lr-abstractions`, Task 5 extended here: `is_alive(pid, start) -> bool`). Produces: `Hub::new(HubDeps)`; submit → profile read from work tree or commit → order(s) → outbox until `Accepted`; broker choice by `hub-policy.toml` (`[brokers] order, strategy, broker_zone_order, query_timeout_s, query_attempts, zone_query, query_retry_s`, `[outbox] ttl_hours, on_expire`); replacement search for ordinary orders only; `Done` after JobId; reaper (two checks 2 s apart; `--detach` skips); handles `temp/runner/<job-id>.json`; log copy; `cancel`/`jobs --mine`/`wait`.

- [ ] **Step 1:** Failing tests: `submit_returns_jobid_and_keeps_order_in_outbox_until_accepted`, `outbox_entry_dropped_after_ttl_when_sender_alive`, `broker_choice_follows_order_and_zone_preference`, `rejected_ordinary_order_tries_next_broker`, `same_machine_rejection_does_not_search_replacement`, `dead_sender_cancels_queued_and_kills_running`, `detach_survives_dead_sender`, `wait_interrupt_cancels_nothing`.
- [ ] **Step 2-4:** Implement, run, commit `feat(hub): клиентская сторона, outbox, reaper`.

### Task 15: lr-obs: log sinks and metrics

**Files:**
- Create: `crates/lr-obs/src/{file.rs,console.rs,oslog.rs,otlp.rs,redact.rs,metrics.rs}`
- Test: `crates/lr-obs/tests/{redact.rs,file.rs}`

**Interfaces:** `LogSink` implementations and metrics from spec 12; field redaction; `tracing` facade used by all roles, sinks attached only here.

- [ ] **Step 1:** Failing tests: `secret_values_redacted`, `file_sink_rotates_by_policy`, `metrics_names_match_spec_12`. **Step 2-4:** Implement, run, commit `feat(obs): получатели логов и метрики`.

### Task 16: lr-dashboard

**Files:**
- Create: `crates/lr-dashboard/src/{server.rs,sse.rs,page.html,snapshot.rs,actions.rs}`
- Test: `crates/lr-dashboard/tests/{server.rs,snapshot.rs}`

**Interfaces:** axum server over `DataSource`, loopback only (`127.0.0.1`, `::1`, no widening); Queue, History, live log, cancel, Stats, Runners, filters, export, HTML snapshot (spec 9); actions gated as spec 9.4.

- [ ] **Step 1:** Failing tests on a fake `DataSource`: `binds_loopback_only`, `non_loopback_host_header_refused`, `queue_page_lists_jobs_with_project_source_label`, `cancel_action_requires_token`, `snapshot_is_single_file`. **Step 2-4:** Implement, run, commit `feat(dashboard): страница и сервер`.

### Task 17: lr-cli: assembly and commands

**Files:**
- Create: `crates/lr-cli/src/{main.rs,build.rs,commands/*.rs,ipc_client.rs,install.rs,init.rs,doctor.rs,roles.rs}`
- Test: `crates/lr-cli/tests/{commands.rs,wiring.rs}`

**Interfaces:** commands per spec 11.1 (`init`, `install`, `uninstall`, `autostart`, `doctor`, `policy check`, `hub run`, `broker run`, `runner run`, `submit`, `run`, `wait`, `status`, `jobs`, `cancel`, `join`, ...); `build.rs` is the only place that constructs `lr-net`, `lr-store`, `lr-exec`, `lr-obs` implementations and passes them to roles; service install under a dedicated unprivileged account, state dir owner = broker owner (spec 10.3).

- [ ] **Step 1:** Failing tests: `submit_without_as_refused`, `status_json_has_project_source_label`, `admin_command_over_network_not_accepted`, `all_roles_start_from_wiring_on_loopback`. **Step 2-4:** Implement, run, commit `feat(cli): сборка ролей и команды`.

### Task 18: Skills, plugin, README

**Files:**
- Modify: `plugin/` skills per spec 11.2, `README.md`

- [ ] **Step 1:** Write skills from spec 11.2 and README (build, install, quick start; Mermaid for diagrams). **Step 2:** Run `cargo run -p lr-cli -- --help` and verify every documented command exists. **Step 3:** Commit `docs: скилы, плагин, README`.

### Task 19: End-to-end and final verification

**Files:**
- Create: `crates/lr-cli/tests/e2e/{single_machine.rs,two_machines.rs,failures.rs}`

- [ ] **Step 1:** Tests using real implementations: `single_machine_submit_wait_passes_and_history_has_one_row`, `join_runner_by_code_then_job_runs_remote`, `kill_runner_mid_job_marks_lost_after_deadline` (with short timeout and `lost_min` override), `broker_restart_keeps_queue`, `timeout_kills_grandchildren`.
- [ ] **Step 2:** Run `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check licenses && cargo test --workspace`. Expected: all pass on Windows (and Linux when available).
- [ ] **Step 3: Coverage audit.** Walk `docs/architecture.html` section by section (3, 4, 5, 6, 7, 9.4, 10.3, 12, 14, 15 and every scenario of 16) and write `temp/coverage.md`: one row per requirement with the test that covers it. Rows without a test are gaps: write the missing tests (or, for a genuinely untestable item, list it for the user). Expected: zero uncovered rows before the final review.
- [ ] **Step 4:** Commit `test: сквозные сценарии и аудит покрытия требований`; then whole-branch review and `superpowers:finishing-a-development-branch` only on the user's command.
