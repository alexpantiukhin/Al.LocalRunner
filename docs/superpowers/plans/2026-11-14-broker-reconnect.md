# Broker Reconnect Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hub and runners that stay alive while the broker restarts reconnect on their own, and when they are not connected, `runners`, `status`, `up` and `doctor` say so directly instead of printing an empty list or "already running".

**Architecture:** Reproduce first: an end-to-end test restarts the broker while the hub and a runner keep running. The code already seems to reconnect (hub `crates/lr-hub/src/client.rs` `run()`, pause 1 s doubling to 30 s, unbounded; runner `crates/lr-runner/src/agent.rs` `run()` plus `connect.rs`, same pause), so the fix tasks are conditional on that test. Independently of the result, the link state ("connected" or "not connected since T") is exposed from the hub (`BrokerStatus`) and from each runner (its local control channel) to the CLI, and printed by `runners`, `status`, `up` and `doctor`.

**Tech Stack:** Rust workspace (tokio, serde); tests are `#[tokio::test]` with fakes (`lr-hub/tests/common`, `lr-runner/tests/common`) and in-process real roles (`crates/lr-cli/tests/e2e`).

**Spec:** issue #16 and the decisions below.

Decisions (approved, do not reopen):
- Reproduce first. Task 1 is the test. Tasks 2 and 3 are fixes that exist only if Task 1 fails; if Task 1 passes on its first run, keep it as a regression test and skip Tasks 2 and 3.
- Reconnection stays unbounded (pause 1 s doubling up to 30 s). Do NOT add a bounded number of retries.
- Honest messages are decided to follow the test result: Tasks 4 to 6 are done in either case.
- `localrunner runners` and `status` print `broker <name> unreachable since <time>` instead of an empty list when the hub is alive but not connected to the broker. `up` and `doctor` say a hub or runner process is "alive but not connected to the broker" instead of only "already running".
- The "restart the hub" advice in today's message (`view::unreachable_line`) is removed: the hub retries by itself.

## Global Constraints

- GitHub texts (issue/PR titles and bodies, comments, commit messages) in English. No agent attribution (`Co-Authored-By`, "Generated with") in commits and PR text.
- No code comments except doc comments on public items. Files stay under 500 lines (`crates/lr-cli/src/doctor.rs` is 454 lines: new doctor code goes into a new module, not into that file; `crates/lr-cli/src/commands/mod.rs` is already over 500: do not touch it).
- Generic core: the hub, broker and runner code names no consumer, language or toolchain.
- No safety checks on submitter-supplied values (runs are local).
- Docs are changed in pairs in the same commit: `docs/architecture.html` + `docs/architecture.en.html`, `README.md` + `README.ru.md`. Do not add design Markdown files.
- Serialized protocol additions are backward compatible: new fields are `Option` with `#[serde(default, skip_serializing_if = "Option::is_none")]` (the convention of `crates/lr-proto/src/ipc.rs`); `lr-proto` messages are listed in the `message!` macro lists at the end of that file. An unknown message type on a local channel is not an error for the receiver: `receive` returns `Received::Ignored` and the runner's control channel answers `ControlReply::Failed`, so a new CLI talking to an old runner gets `Failed` and must treat it as "no information".
- `crates/lr-proto/src/ipc.rs` is 471 lines today and Tasks 4 and 5 add about 20: stay under 500 and add nothing else to it.
- Test commands run per package while iterating (`cargo test -p <crate> --test <name> <filter>`). Before each commit run `cargo fmt --all --check` and `cargo clippy -p <crate> --all-targets --locked -- -D warnings`; CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked --no-fail-fast` (`.github/workflows/ci.yml`), so for commits that touch `lr-proto` (Tasks 4 and 5) run clippy and tests for the whole workspace, because `BrokerStatus`, `RunnerControl` and `ControlReply` are used by `lr-hub`, `lr-runner`, `lr-cli`, `lr-dashboard` and by test fixtures of several crates.
- Every new public item (type, variant, field, function) gets a doc comment; nothing else gets comments.

## Review Focus

- The broker is down for a long time (many backoff steps, pause at 30 s): `runners` must still say `unreachable since <time>` with the time of the first loss, not of the latest attempt (Task 4 test).
- The hub never connected since its start (broker started later): `unreachable since` shows the hub start time rather than nothing (Task 4 test).
- A runner that has never been welcomed since its start (broker down at start) reports "not connected" with its start time (Task 5 test).
- A runner whose control channel does not answer (old build without `status`): `up` and `doctor` print "already running" without a link line and do not fail (Task 6 test).
- `--json` output of `brokers` keeps working and carries the new `since` field; an old hub reply without `since` parses (Task 4 test).

## File Structure

- Create: `crates/lr-cli/tests/e2e/broker_restart.rs` (Task 1).
- Modify: `crates/lr-cli/tests/e2e/main.rs` (add `mod broker_restart;`).
- Modify: `crates/lr-proto/src/ipc.rs` (`BrokerStatus.since`, `RunnerControl::Status`, `ControlReply::Link`).
- Modify: `crates/lr-hub/src/track.rs`, `crates/lr-hub/src/inbound.rs`, `crates/lr-hub/src/cli.rs`, `crates/lr-hub/src/engine.rs` (record and report the time of the last link change).
- Create: `crates/lr-hub/tests/broker_link.rs`.
- Modify: `crates/lr-runner/src/agent.rs` (`AgentControl::link`), `crates/lr-cli/src/control.rs` (answer `Status`).
- Create: `crates/lr-cli/src/link_report.rs` (shared lines for `up` and `doctor`); register in `crates/lr-cli/src/lib.rs`.
- Modify: `crates/lr-cli/src/commands/view.rs` (`unreachable_line`, `reachable`), `crates/lr-cli/src/commands/up_cmd.rs`, `crates/lr-cli/src/doctor.rs` (one call into `link_report`).
- Create: `crates/lr-cli/tests/link_messages.rs`, `crates/lr-runner/tests/link_state.rs`.
- Modify docs: `docs/architecture.html`, `docs/architecture.en.html`, `README.md`, `README.ru.md` (Task 7).

---

### Task 1: Reproduce - broker restart with hub and runner alive

**Files:**
- Create: `crates/lr-cli/tests/e2e/broker_restart.rs`
- Modify: `crates/lr-cli/tests/e2e/main.rs` (add `mod broker_restart;` in alphabetical position)

**Interfaces:**
- Consumes: from `crate::support`: `layout`, `add_runner`, `repo`, `submit`, `until_exit`, `eventually`, `runner_online`, `broker_available`, `runners`, `remove`, `toml_str`; from `lr_cli::roles`: `start_broker(&Layout, bool) -> Result<Running, String>`, `start_hub`, `start_runners(&Layout, Option<&str>, u32, Option<&Path>, bool)`; `Running::stop(self)`. Patterns to copy: `crates/lr-cli/tests/e2e/single_machine.rs` (one machine, one runner `r1`, a submitted profile), `failures.rs::broker_restart_keeps_queue` (already does `broker.stop()`, then `start_broker` on the same layout with hub and runner alive, then `runner_online`) and `dashboard.rs` (after `broker.stop()` the hub reports the broker unavailable within 60 s). `moved_broker.rs` is NOT the pattern for a lone broker restart: it stops the hub too and starts a second layout.
- Harness facts (verified in the code): the e2e tests run hub, broker and runners as tasks inside the test process, on Windows and on Linux alike. `Running::stop` aborts the role's tasks and closes its IPC listeners (for the broker: the broker IPC and the admin IPC; a Unix socket file is removed), so the hub and the runner stay alive and their dials fail until `start_broker` binds the same IPC addresses again. Connections accepted before the stop end on their own (the broker's per-connection writer ends once the engine is gone), which is what the 60 s `eventually` below waits for. The existing `broker_restart_keeps_queue` already passes this way, so a first-run pass of the new tests is the expected outcome. What the harness cannot cover: separate OS processes, a real OS service (`Restart-Service`), and `[listen]` (local hub and runners dial the broker over IPC, `Layout::broker_ipc`; the TLS listener from `[listen]` is bound only while a join code is open or a machine is registered, which a one-machine layout never has).
- Produces: regression tests only.

- [ ] **Step 1: Write the tests**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_and_runner_reconnect_after_the_broker_restarts()
```
Body: layout `machine` with runner `r1` (`add_runner`); start broker, hub, runner; `runner_online(&layout, "r1")`; `broker.stop()`; `eventually("the hub to see the broker gone", 60 s, || async { broker_available(&layout).await.map_or(Some(()), |_| None) })`; start the broker again on the same layout; `eventually("the hub to reconnect", 90 s, || broker_available(&layout))` (assertion A: the hub came back); `runner_online(&layout, "r1")` (assertion B: the runner came back and `runners` lists it online); then `repo(&dir, ...)` with profile `unit` (`command = "echo reconnected"`, `timeout_s = 60`, `mem_mb = 64`), `submit`, and assert `until_exit(...).state == JobState::Passed` (assertion C). Finish with `runners.stop()`, `hub_role.stop()`, `broker.stop()`, `remove(dir).await` as `single_machine.rs` does.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_and_runner_reconnect_after_a_listen_change_and_restart()
```
Body: same, but before restarting the broker rewrite `config_file("policy.toml")` of the layout so that `[listen] address` (the key `support::layout` writes is `address = "127.0.0.1:0"`) names another free loopback port (bind `127.0.0.1:0`, read the port, drop the listener; then `.replace("127.0.0.1:0", &address)` as `moved_broker.rs` does), and assert the same outcomes A, B, C. The local hub and the local runner reach the broker over IPC (`crates/lr-cli/src/build.rs` `ipc_dialer`, `Layout::broker_ipc`), so the network address must not matter; this test only pins that independence and is expected to pass by construction. It does not cover a hub or runner joined over the network to a broker whose `[listen]` address changed: those dial the address pinned at join and cannot follow a moved listener by design (owner question 2 in Self-Review).

- [ ] **Step 2: Run the tests**

Run: `cargo test -p lr-cli --test e2e broker_restart -- --test-threads=1`
Expected: record the result of each test and the hub/runner diagnostic log lines (`broker unreachable`, `broker connected`, `broker silent, reconnecting`) from the layout's log directory when a test fails.

- [ ] **Step 3: Apply the decision rule**

A failure counts only if it repeats on a second run of the same test alone (`--test-threads=1`); one timeout of a 60 or 90 s wait on a loaded machine is not a finding.
- Both tests pass: keep them as regression tests, skip Tasks 2 and 3, go to Task 4. Record in the PR text that the scenario did not reproduce in-process and that the process and service boundary is not covered by the harness (owner question 1 in Self-Review).
- Assertion A fails (`timed out waiting for the hub to reconnect`): do Task 2.
- A passes and B fails (`timed out waiting for runner r1 online`): do Task 3.
- A and B pass and C fails: the links are back but jobs do not flow; bisect with the log lines (`broker connected`, `broker silent, reconnecting`), name the layer found and do Task 2 or 3 for it.
- A and B both fail: do Task 2 first, rerun, then Task 3 only if B still fails.
Do not change a test to make it pass.

- [ ] **Step 4: Commit**

```bash
git add crates/lr-cli/tests/e2e/broker_restart.rs crates/lr-cli/tests/e2e/main.rs
git commit -m "test: hub and runner reconnect after the broker restarts"
```

---

### Task 2 (conditional: only if Task 1 shows the hub does not return): hub reconnect

**Files:**
- Modify: `crates/lr-hub/src/client.rs` (the cause found; 304 lines), possibly `crates/lr-hub/src/inbound.rs`
- Test: `crates/lr-hub/tests/restart.rs` (existing rig in `tests/common`: `Rig`, `FakeBroker`, `eventually`, `hellos()`)

**Interfaces:**
- Consumes: `FakeBroker::disconnect()`, `FakeBroker::hellos() -> usize`, `Rig::local_broker`.
- Produces: none.

- [ ] **Step 1: Write a failing unit test at the layer where the e2e failure was localized**, named after the cause (for example `hub_redials_after_the_broker_listener_is_replaced`): the fake broker listener is closed and a new one listens on the same address; assert `eventually("reconnected", || broker.hellos() == 1)` on the new broker and `brokers[0].available` through `CliRequest::Brokers`.
- [ ] **Step 2: Run it and see it fail**: `cargo test -p lr-hub --test restart <name>` Expected: FAIL on the reconnect assertion.
- [ ] **Step 3: Fix the cause in `client.rs::run`/`session`** (the pause must stay 1 s doubling to 30 s, no retry limit; a session that ended after a welcome resets the pause to 1 s as today).
- [ ] **Step 4: Run** `cargo test -p lr-hub` Expected: PASS; then rerun Task 1 tests, Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "fix(hub): redial the broker after <cause>"` (add the changed files).

---

### Task 3 (conditional: only if Task 1 shows the runner does not return): runner reconnect

**Files:**
- Modify: `crates/lr-runner/src/agent.rs` (`run`, `connect_once`) or `crates/lr-runner/src/connect.rs` (the cause found)
- Test: `crates/lr-runner/tests/agent.rs` (existing: `job_runs_and_result_reaches_broker_once_after_reconnect`, `silent_broker_is_dropped_after_45_seconds_and_redialed`)

**Interfaces:**
- Consumes: the runner test rig in `crates/lr-runner/tests/common`.
- Produces: none.

- [ ] **Step 1: Write a failing test named after the cause** (for example `runner_redials_when_the_broker_closes_during_welcome`) in `agent.rs`: the fake broker drops the connection at the given point, then accepts; assert a second `hello` arrives and a job assigned after it runs.
- [ ] **Step 2: Run it**: `cargo test -p lr-runner --test agent <name>` Expected: FAIL.
- [ ] **Step 3: Fix the cause.** Keep pause 1 s doubling to 30 s, unbounded; keep the kill deadline counted from the loss of a welcomed session.
- [ ] **Step 4: Run** `cargo test -p lr-runner` Expected: PASS; rerun Task 1 tests, Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "fix(runner): redial the broker after <cause>"`.

---

### Task 4: Hub reports "unreachable since" (hub side and `runners`/`status`/`brokers`)

**Files:**
- Modify: `crates/lr-proto/src/ipc.rs` (`BrokerStatus`)
- Modify: `crates/lr-hub/src/track.rs` (`Link`), `crates/lr-hub/src/inbound.rs` (`on_broker`), `crates/lr-hub/src/cli.rs` (`brokers()` near line 408), `crates/lr-hub/src/engine.rs` (where `client::spawn` is called for each target, around line 184)
- Modify: `crates/lr-cli/src/commands/view.rs` (`unreachable_line`, `reachable`)
- Test: `crates/lr-hub/tests/broker_link.rs`, `crates/lr-cli/tests/link_messages.rs`

**Interfaces:**
- Consumes: `Timestamp` (`lr_core`, milliseconds, `#[serde(transparent)]`), `format_time(Timestamp) -> String` (`crates/lr-cli/src/commands/timefmt.rs`, prints UTC), the hub's clock (`Shared.clock.now()`, reachable as `self.s.clock` in `Engine`), `Rig.clock: ManualClock` and `Rig::step(secs)` in `crates/lr-hub/tests/common/mod.rs` (the hub tests have a controllable clock).
- Produces: `BrokerStatus { name, zone, available, outbox, since: Option<Timestamp> }` where `since` is the time `available` last changed, or the hub start time while it never connected (`#[serde(default, skip_serializing_if = "Option::is_none")]`); `pub fn unreachable_line(name: &str, since: Option<Timestamp>) -> String`.
- Other places that build `BrokerStatus` literals and must get `since: None` (their crates stop compiling otherwise): `crates/lr-proto/tests/common/ipc.rs`, `crates/lr-proto/tests/dashboard_model.rs`, `crates/lr-dashboard/tests/common/mod.rs`, `crates/lr-cli/tests/dashboard_details.rs`, `crates/lr-cli/tests/dashboard_source.rs`; find any other with `rg "BrokerStatus \{" crates`. `DashboardSnapshot.brokers` carries `BrokerStatus`, so the dashboard JSON gains `since` when set; the page (`crates/lr-dashboard/src/page.js`) ignores it.

- [ ] **Step 1: Write failing hub tests** in `crates/lr-hub/tests/broker_link.rs` (read `Rig` in `tests/common/mod.rs` first and use its fake clock):
  - `since_is_the_hub_start_while_the_broker_never_answered`: no broker listening; `CliRequest::Brokers` returns the local broker with `available == false` and `since == Some(start)`.
  - `since_moves_to_the_loss_when_a_connected_broker_goes_away`: connect, advance the clock, `FakeBroker::disconnect()`; assert `available == false` and `since` equals the time of the loss.
  - `since_stays_at_the_first_loss_across_failed_redials`: after the loss advance the clock over several backoff steps without a broker; assert `since` is unchanged.
  - `since_moves_to_the_return_when_the_broker_comes_back`: assert `available == true` and `since` equals the time of the new `hello`.
- [ ] **Step 2: Write failing CLI tests** in `crates/lr-cli/tests/link_messages.rs`:
  - `unreachable_line_names_the_broker_and_the_time`: `unreachable_line("local", Some(t))` equals `format!("broker local unreachable since {}", format_time(t))` followed by ` (the hub retries on its own)`; contains neither `restart the hub` nor `down hub`.
  - `unreachable_line_without_a_time_still_names_the_broker`: with `None` the text is `broker local unreachable (the hub retries on its own)`.
  - `old_hub_reply_without_since_parses`: JSON `{"name":"local","zone":"local","available":false,"outbox":0}` deserializes into `BrokerStatus` with `since == None`.
- [ ] **Step 3: Run them and see them fail**: `cargo test -p lr-hub --test broker_link` and `cargo test -p lr-cli --test link_messages` Expected: FAIL (compile errors on the missing field and the new signature).
- [ ] **Step 4: Implement.** `BrokerStatus.since` as above. `Link` (`track.rs`, `#[derive(Default)]`) gets `since: Option<Timestamp>`. Links are created in two places in `engine.rs`: `targets()` (joined brokers, `Link { zone, ..Link::default() }`, around line 253) and the loop that spawns the clients (`engine.links.entry(t.name.clone()).or_default()`, around line 184, which also creates the `local` link); set `since = Some(now)` in that loop for every target (one place covers both) with the clock the hub got (`d.clock.now()`, before `d.clock` is moved into the ticker task). `on_broker` sets `since = Some(self.s.clock.now())` on `Connected` and on `Disconnected`, and not on `Refused`; failed dials never reach the engine, so repeated failures keep the first loss time. `brokers()` in `cli.rs` (around line 408) fills `since: l.since`. In `view.rs` change `unreachable_line` to the two forms tested above and pass `b.since` at both callers (`reachable`, line ~187, and the `status` path, line ~259); fix the third caller `up_cmd.rs::hub_unlinked` (line 84) by passing `b.since` (it is replaced in Task 6). No existing test asserts the old text (`is unreachable ... restart the hub`); checked by grep.
- [ ] **Step 5: Run** `cargo test -p lr-hub` and `cargo test -p lr-cli --test link_messages` and `cargo test -p lr-cli --test e2e broker_restart` Expected: PASS.
- [ ] **Step 6: Commit** `git commit -m "feat(hub): report since when a broker is unreachable, in runners, status and brokers"`.

---

### Task 5: Runner reports its link state on its control channel

**Files:**
- Modify: `crates/lr-proto/src/ipc.rs` (`RunnerControl::Status`, `ControlReply::Link`, `message!` lists)
- Modify: `crates/lr-runner/src/agent.rs` (`AgentControl`), `crates/lr-cli/src/control.rs`
- Test: `crates/lr-runner/tests/link_state.rs` (rig: `tests/common/mod.rs` `Rig`, `Rig::start() -> (AgentControl, JoinHandle<AgentExit>)`, `Broker::accept(&rig)`, `rig.advance`, `rig.clock`), and the proto fixtures `controls()` and `control_replies()` in `crates/lr-proto/tests/common/ipc.rs` (the round trips in `tests/messages.rs` iterate them: add the new variants there instead of a separate round-trip test)

**Interfaces:**
- Consumes: `Shared.state().lost_since: Option<Timestamp>` (set to the start time in `Agent::new`, cleared by `set_lost(None)` on `welcome`, set on every loss), `Agent::control() -> AgentControl`.
- Produces: `RunnerControl::Status` (wire `status`), `ControlReply::Link { connected: bool, since: Option<Timestamp> }` (wire `link`, `since` is the start of the current break, `None` when connected), `AgentControl::link(&self) -> (bool, Option<Timestamp>)`.

- [ ] **Step 1: Write failing tests.**
  - `crates/lr-runner/tests/link_state.rs`: `a_runner_that_never_met_the_broker_is_not_connected_since_its_start` (no broker listening; `control.link()` is `(false, Some(start))`); `a_welcomed_runner_is_connected` (`(true, None)`); `after_the_broker_goes_away_the_break_starts_at_the_loss` (`(false, Some(loss_time))` and the time does not move over later failed dials).
  - proto: add `RunnerControl::Status` to `controls()`, and `ControlReply::Link { connected: false, since: Some(..) }` and `ControlReply::Link { connected: true, since: None }` to `control_replies()` (the existing round trips then cover them); add one test next to the neighbouring ones in `tests/messages.rs` that `{"type":"link","connected":true}` (no `since`) parses with `since == None`.
- [ ] **Step 2: Run** `cargo test -p lr-runner --test link_state` and the proto test. Expected: FAIL (missing items).
- [ ] **Step 3: Implement.** `AgentControl` (a tuple struct over `mpsc::Sender<Control>`, built only in `RunnerAgent::control`) becomes a struct holding the sender and an `Arc<Shared>`; `link()` reads `lost_since` under `Shared::state()` and returns `(lost_since.is_none(), lost_since)`. In `control.rs::connection` answer `RunnerControl::Status` with `ControlReply::Link` from `control.link()`. Add `"status"` to the `RunnerControl` list and `"link"` to the `ControlReply` list in the `message!` macros.
- [ ] **Step 4: Run** `cargo test -p lr-runner -p lr-proto` Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "feat(runner): report the broker link state on the control channel"`.

---

### Task 6: `up` and `doctor` say "alive but not connected to the broker"

**Files:**
- Create: `crates/lr-cli/src/link_report.rs`; register `pub mod link_report;` in `crates/lr-cli/src/lib.rs`
- Modify: `crates/lr-cli/src/commands/up_cmd.rs` (remove `hub_unlinked`, use the module; runners loop near line 298), `crates/lr-cli/src/doctor.rs` (`checks_with` gets one call)
- Test: `crates/lr-cli/tests/link_messages.rs` (extend), `crates/lr-cli/tests/e2e/broker_restart.rs` (extend)

**Interfaces:**
- Consumes: `known_brokers(&IpcClient)`, `Layout::hub_ipc(&principal)`, `Layout::runner_ipc(&principal, &name)`, `IpcClient::connect(path, IpcChannel::RunnerControl)` and `.call::<_, ControlReply>(&RunnerControl::Status)` (as `down_cmd.rs` line ~109 calls the control channel), `lr_net::current_os_principal()` (as `up_cmd.rs::hub_unlinked` does), `Layout::runner_names()`, `format_time`.
- Produces: `pub fn not_connected_line(who: &str, since: Option<Timestamp>) -> String` returning `"{who}: alive but not connected to the broker since {time}"` (without `since`: `"{who}: alive but not connected to the broker"`); `pub async fn hub_lines(layout: &Layout) -> Vec<String>` (one line per broker the running hub is not connected to, named `hub (broker {name})`); `pub async fn runner_line(layout: &Layout, name: &str) -> Option<String>` (`Some` only when the runner answers `Link { connected: false, .. }`; `None` when it is connected, does not answer, or answers `Failed` as an old build does); `pub async fn checks(layout: &Layout) -> Vec<crate::doctor::Check>` with status `Warn`, names `hub link` and `runner {name} link`, built with the `Check { name, status, detail }` literal (its fields are public; `doctor::check` is private), over `layout.runner_names()`; it yields nothing when the hub or a runner does not answer on its channel, so a node with nothing running and the existing `doctor_checks.rs` tests see no new check. `hub_lines` replaces `up_cmd.rs::hub_unlinked` (the hub half of this already exists today as `hub_unlinked` plus `unreachable_line`).

- [ ] **Step 1: Write failing tests.**
  - `not_connected_line_names_the_process_and_the_time` and `not_connected_line_without_time` (exact strings above).
  - `a_runner_that_answers_failed_to_status_gives_no_line`: a fake control listener answers `ControlReply::Failed`; `runner_line` returns `None`.
  - In `broker_restart.rs`: `up_and_doctor_name_a_runner_that_is_alive_but_not_connected`: start only a runner (no broker) on a layout; `runner_line(&layout, "r1")` is `Some(..)` containing `alive but not connected to the broker`; `link_report::checks(&layout)` holds a `Warn` named `runner r1 link`; after starting the broker and `runner_online`, `runner_line` is `None` and no link warning remains. First check that `start_runners` works while no broker is listening (it needs only `runner.toml`; the broker address defaults to `Layout::broker_ipc`); if it does not, start the broker, wait for `runner_online`, then `broker.stop()` and wait until the runner's link is reported as lost. Second test, `hub_lines_name_a_broker_that_went_away`, on the Task 1 flow: with hub and runner alive, `broker.stop()`, wait until `broker_available` is gone, assert `hub_lines(&layout)` has a line containing `alive but not connected to the broker`, restart the broker, wait for `broker_available`, assert `hub_lines` is empty.
- [ ] **Step 2: Run** `cargo test -p lr-cli --test link_messages` and `cargo test -p lr-cli --test e2e up_and_doctor` Expected: FAIL.
- [ ] **Step 3: Implement the module.** `up_cmd.rs`: in the "already running" branches print `hub: already running` and the `hub_lines` to stderr as today (wording changed), and after `runner {r}: already running` print `runner_line` to stderr when `Some`; exit code unchanged. `doctor.rs::checks_with`: `out.extend(crate::link_report::checks(layout).await);` after `runner_versions`. Keep `doctor.rs` under 500 lines.
- [ ] **Step 4: Run** `cargo test -p lr-cli` Expected: PASS (all existing `up`/`doctor` tests unchanged).
- [ ] **Step 5: Commit** `git commit -m "feat(cli): up and doctor report a hub or runner that is alive but not connected to the broker"`.

---

### Task 7: Documentation (both languages, in the same commit)

**Files:**
- Modify: `docs/architecture.html` and `docs/architecture.en.html`: the "Connection" item (en line ~1207: add that reconnection is unbounded and that a runner reports "not connected since" on its local control channel, command `status`); the runner control channel item (en line ~464: add `status`); the `runners`, `status`, `up` and `doctor` rows of the command table (en lines ~958, 963, 970, 994) and the `doctor` description (en line ~883): the new messages and the check names `hub link`, `runner <name> link`; the hub's `brokers` description if present (`since`).
- Modify: `README.md` and `README.ru.md`: after the quick-start lines (`localrunner runners`, around line 104-106) one short note: if `runners` prints `broker <name> unreachable since <time>`, the hub and runners reconnect by themselves with a growing pause (1 to 30 s); `up` and `doctor` show a process that is alive but not connected.

- [ ] **Step 1:** Edit the Russian and English architecture files together and keep their section numbers and anchors identical; edit both READMEs together.
- [ ] **Step 2:** Verify the pairs: `git diff --stat` shows both files of each pair changed; grep each language for the same new command names (`rg -c "not connected" docs README.md README.ru.md`) and both HTML files stay well-formed (open the changed sections once and check tags are balanced).
- [ ] **Step 3: Commit** `git commit -m "docs: broker reconnection and link state messages"`.

---

### Task 8: Delivery

- [ ] **Step 1:** Run the CI set on the whole workspace: `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked --no-fail-fast` (spec-coverage and skills-lint tests are part of it). Check `wc -l` of every changed `.rs` file is under 500; the close ones are `crates/lr-proto/src/ipc.rs` (471), `crates/lr-hub/src/engine.rs` (477), `crates/lr-hub/src/cli.rs` (452), `crates/lr-cli/src/doctor.rs` (454).
- [ ] **Step 2:** Push `fix/broker-reconnect` and open one pull request to `main`. Title and body in English, `Closes #16`, no agent attribution lines. The body states the Task 1 result (reproduced or not, which assertion), that reconnection stays unbounded with the 1 s to 30 s backoff, and the new messages.

## Self-Review

- Spec coverage: reproduction (Task 1, including the `[listen]` change), conditional fixes (Tasks 2, 3), `runners`/`status` honest message (Task 4), `up`/`doctor` (Tasks 5, 6), docs in both pairs (Task 7). Bounded retries intentionally absent (decision).
- Type consistency: `BrokerStatus.since: Option<Timestamp>` (Task 4) is used by `reachable` and `link_report::hub_lines` (Task 6); `ControlReply::Link { connected, since }` and `AgentControl::link()` (Task 5) are used by `link_report::runner_line` (Task 6); `unreachable_line(name, since)` signature is the same in Tasks 4 and 6.
- Resolved from the code: the hub `Rig` has `clock: ManualClock` and `step(secs)`; the `[listen]` key of the test layout is `address`; no existing test asserts the old unreachable text.
- Owner questions: (1) the in-process e2e cannot cross a process or OS-service boundary; if Task 1 passes, is that enough to close the reproduction, or should an out-of-process check with the built binary (`up`, then `down broker` and `up broker`, or `Restart-Service`) be added? It is slower and platform-specific. (2) A hub or runner joined over the network dials the address pinned at join; after a `[listen]` change it cannot reconnect by design and will now report `unreachable since`: is that message enough, or should it say the address may have changed?
