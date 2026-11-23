# Change every setting through the CLI (#136) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every setting of runner.toml, policy.toml, hub-policy.toml and logging.toml is changeable through `localrunner` commands on the local machine, with diff, validation, backup, atomic write, self-elevation and reload; [listen], [networks] and [[shared]] apply live.

**Architecture:** A new `config_edit` module in lr-cli is the one engine: resolve the file a role reads, edit it with `toml_edit` (comments and order kept), validate with the existing parsers, build a diff, write backup + atomic file, elevate by re-running the same command when the directory needs rights, then reload through the existing role channels. `config set|get|unset|show` and the `policy` list verbs are thin front ends over it. Live apply is a separate change in lr-broker and the lr-cli broker assembly.

**Tech Stack:** Rust workspace (lr-cli, lr-broker, lr-hub, lr-obs, lr-runner), `toml_edit` (new), `toml 0.8`, clap.

**Spec:** `temp/2026-11-23-config-via-cli-design.md` (approved by the owner, issue #136). One branch, one pull request, commits by stage.

## Global Constraints

- Generic core: lr-broker/lr-core never name a consumer, language or toolchain (AGENTS.md).
- Every cargo build, check, test, clippy and doc runs through `localrunner` (skill `localrunner:run`; profiles in `.localrunner/profiles.toml`: `rust-fmt`, `rust-clippy`, `rust-unit`, `rust-test` per package; `submit --detach` then `wait`). Never call `cargo` by hand. Subagents get this rule in their prompt.
- Tests do not wait on real time: synchronize on events, `start_paused` and the manual clock; no sleeps or polling.
- No code comments except doc comments on public items. No new files unless needed. Temporary files go to `temp/`. Never read `secrets/`.
- GitHub text (commits, PR, comments) in English; no agent attribution (`Co-Authored-By`, "Generated with") in commits and PR.
- Commands change only this machine's configuration; nothing is sent to a hub, broker or runner on another machine.
- No human-confirmation gate for `allow_elevated` and `allow_same_user_as_broker`; the diff and the result print a separate line "widens privileges: <key> ..." for them. Skills never propose these keys.
- No key registry: type of an existing key wins, a new value is parsed as a TOML value and falls back to a string, `--string` forces a string.
- Elevation is one prompt: validate and print the diff unelevated, then re-run the same command elevated for write and reload. Never trigger a real UAC or sudo prompt from a test or from the agent without asking the user first (global user rule); tests use a fake `Elevator`.
- `--restart` is the only way a command restarts a role; the output warns that connections drop.
- The plugin follows the behavior: skills and `plugin/shared/os/*.md` change in the same PR; `docs/architecture.html` and `docs/architecture.en.html`, `README.md` and `README.ru.md` change together.
- The PR is created only after the local twins of its CI jobs are green (AGENTS.md): `rust-fmt`, `rust-clippy`, `rust-unit`, `ci-docs`, `skills-lint` as the changed paths require. The owner accepts locally before any push or PR.

## Review Focus

- A key whose file is unreadable or absent (no policy.toml yet): `config set` creates it; `get` says "not set".
- A value that parses as a different type than the schema wants (`"5"` vs `5`): the real parser refuses, the file is untouched, and the message names the key.
- A comment-heavy hand-edited file: comments and order survive `set` and `unset` byte for byte outside the changed lines.
- `allow-secret` with an empty label: accepted, warns; running it twice changes nothing and exits 0.
- Elevated child fails or the user refuses UAC: no partial write, no backup left without its original, exit code non-zero.
- Two edits of the same file racing: the second sees the first's result or fails; the file is never half-written (atomic rename).
- A role that is not running: the file is written and reload is reported "not running", exit 0.
- `runner.toml` typo (`runner x.typo 1`): refused, not silently written.

---

### Task 1: Document editing core and value typing

**Files:**
- Modify: `Cargo.toml` (workspace: `toml_edit = "0.22"`), `crates/lr-cli/Cargo.toml`
- Create: `crates/lr-cli/src/config_edit/mod.rs`, `crates/lr-cli/src/config_edit/document.rs`, `crates/lr-cli/src/config_edit/value.rs`
- Modify: `crates/lr-cli/src/lib.rs` (`pub mod config_edit;`)
- Test: `crates/lr-cli/tests/config_edit_document.rs`

**Interfaces:**
- Produces:
  - `pub struct ConfigDoc` with `ConfigDoc::parse(text: &str) -> Result<ConfigDoc, String>`, `render(&self) -> String`.
  - `ConfigDoc::get(&self, key: &str) -> Option<String>` (TOML text of the value), `set(&mut self, key: &str, value: &str, force_string: bool) -> Result<(), String>`, `unset(&mut self, key: &str) -> Result<bool, String>` (true when something was removed). `key` is a dotted path `section.key`; segments with dots use TOML quoting.
  - `pub fn parse_value(text: &str, existing: Option<&toml_edit::Value>, force_string: bool) -> toml_edit::Value`.

- [ ] **Step 1: Write failing tests** in `config_edit_document.rs`: `set_keeps_comments_and_order` (a file with comments and three tables; `set("queue.max", "5")` changes only that line), `new_value_is_parsed_as_toml_then_string` (`"5"`→integer, `true`→bool, `[1,2]`→array, `hello`→string), `existing_type_wins` (existing string key given `5` stores `"5"`), `force_string`, `unset_removes_key_and_empty_table` returns `true`, then `false` the second time, `get_missing_is_none`, `set_creates_missing_table`.
- [ ] **Step 2: Run** `rust-test` for package `lr-cli`, test `config_edit_document`; expect compile failure (module missing).
- [ ] **Step 3: Implement** the signatures above on `toml_edit::DocumentMut`; `parse_value` tries `text.parse::<Value>()`, falls back to a string, and when `existing` is a string and the parse gave a non-string scalar re-reads the text as a string.
- [ ] **Step 4: Run** the same test; expect PASS.
- [ ] **Step 5: Commit** `feat(config): edit TOML documents keeping comments and order (#136)`.

### Task 2: Targets, file resolution and validation

**Files:**
- Create: `crates/lr-cli/src/config_edit/target.rs`, `crates/lr-cli/src/config_edit/validate.rs`
- Modify: `crates/lr-cli/src/commands/admin_cmd.rs` (`policy_check` uses the resolver)
- Test: `crates/lr-cli/tests/config_edit_target.rs`

**Interfaces:**
- Consumes: `Layout` (`crates/lr-cli/src/paths.rs`: `config_file`, `runner_dir`, `broker_is_service`, `is_service`), `service_root()`, `runner_config_path` (`build.rs`).
- Produces:
  - `pub enum Target { Broker, Hub, Runner(String), Logging }` with `FromStr` for `broker | hub | runner:<name> | logging`.
  - `pub fn resolve(layout: &Layout, target: &Target) -> Result<PathBuf, String>`: Broker → `service_root()/policy.toml` when `layout.broker_is_service()`, else `layout.config_file("policy.toml")`; Hub → `hub-policy.toml`; Logging → `logging.toml` of the layout (service layout when the broker is a service and the layout is the service's); Runner(name) → own copy `runner/<name>/runner.toml` when it exists, else the template.
  - `pub fn validate(target: &Target, text: &str, layout: &Layout) -> Result<Vec<String>, String>` returning warnings: Broker → `lr_broker::policy::load` + `check`; Hub → `lr_hub::policy::parse_hub_policy`; Logging → `lr_obs::LoggingConfig::parse`; Runner → `RunnerConfig::parse`, and an `Err` when the edited text has more `warnings` (unknown keys) than the original.

- [ ] **Step 1: Failing tests**: `broker_resolves_to_the_service_file_when_the_service_is_installed` (inject installed flag through a `resolve_with(layout, target, service_installed: bool)` variant), `runner_prefers_own_copy`, `policy_check_without_file_uses_resolved_path` (the issue's case: AppData copy vs service file), `validate_policy_rejects_unknown_key`, `validate_runner_refuses_a_new_unknown_key`, `validate_runner_keeps_old_unknown_key_warning`.
- [ ] **Step 2: Run** (expect FAIL).
- [ ] **Step 3: Implement** both modules; change `policy_check` to call `resolve` for the no-argument path and print the path.
- [ ] **Step 4: Run** `config_edit_target` and the existing `policy_check` tests (`crates/lr-broker/tests/policy_check.rs`, `crates/lr-cli/tests/commands.rs`); expect PASS.
- [ ] **Step 5: Commit** `feat(config): resolve and validate the file a role reads (#136)`.

### Task 3: Change plan, diff, backup and atomic write

**Files:**
- Create: `crates/lr-cli/src/config_edit/change.rs`
- Test: `crates/lr-cli/tests/config_edit_change.rs`

**Interfaces:**
- Consumes: `ConfigDoc`, `Target`, `resolve`, `validate`.
- Produces:
  - `pub struct Change { pub path: PathBuf, pub before: String, pub after: String, pub warnings: Vec<String>, pub widens: Vec<String> }` and `Change::diff(&self) -> String` (unified, 3 lines of context, header names the path).
  - `pub fn plan(layout: &Layout, target: &Target, edit: impl FnOnce(&mut ConfigDoc) -> Result<(), String>) -> Result<Change, String>`: reads the file (missing = empty), applies `edit`, validates, fills `widens` with `allow_elevated` / `allow_same_user_as_broker` when they changed to `true`.
  - `pub fn needs_rights(path: &Path) -> bool` (cannot create a probe file in the directory, like `paths::writable_or`).
  - `pub fn write(change: &Change) -> Result<PathBuf, String>`: backup `<name>.bak-<UTC yyyymmddThhmmss>` beside the file (skipped when the file did not exist), temp file in the same directory, `rename` over the target; returns the backup path or the path itself when no backup.
  - `Change::is_noop(&self) -> bool`.

- [ ] **Step 1: Failing tests**: `diff_names_the_path_and_the_changed_line`, `noop_when_value_equal`, `write_makes_backup_then_replaces`, `write_leaves_original_when_rename_fails` (target is a directory), `widens_lists_allow_elevated`, `plan_refuses_invalid_policy_and_writes_nothing`.
- [ ] **Step 2: Run** (FAIL). **Step 3: Implement.** The UTC stamp comes from the `Clock` abstraction already used in lr-cli (`lr_abstractions::Clock`), passed into `write` so tests use the fake clock.
- [ ] **Step 4: Run** (PASS). **Step 5: Commit** `feat(config): diff, backup and atomic write (#136)`.

### Task 4: Elevation

**Files:**
- Create: `crates/lr-cli/src/config_edit/elevate.rs`
- Test: `crates/lr-cli/tests/config_edit_elevate.rs`

**Interfaces:**
- Produces:
  - `pub trait Elevator { fn run_elevated(&self, exe: &Path, args: &[String]) -> Result<i32, String>; }`
  - `pub struct OsElevator;` implementing it: Windows `powershell -Command Start-Process -Verb RunAs -Wait -PassThru` with output captured through a temp file (the pattern of `elevated_command` in `commands/service_cmd.rs`) and the child's exit code returned; Unix `sudo <exe> <args>`.
  - `pub fn elevated_command_line(os: Os, exe: &Path, args: &[String]) -> String`: the exact command printed when the session cannot elevate (no TTY on Unix for sudo, or the user declines).
  - Elevated child marker: hidden flag `--elevated-child` so the child does not elevate again and skips the diff already shown.

- [ ] **Step 1: Failing tests** with a fake `Elevator`: `elevated_command_line_quotes_arguments_per_os` (Windows and Unix strings), `child_flag_is_appended_once`, `refusal_returns_error_and_exit_code_nonzero`.
- [ ] **Step 2–4:** run (FAIL), implement, run (PASS). Do not execute `OsElevator` in tests.
- [ ] **Step 5: Commit** `feat(config): re-run the command elevated when the file needs rights (#136)`.

### Task 5: Apply, restart handling and the `config` command

**Files:**
- Create: `crates/lr-cli/src/config_edit/apply.rs`, `crates/lr-cli/src/commands/config_cmd.rs`
- Modify: `crates/lr-cli/src/commands/args.rs` (`ConfigAction`), `crates/lr-cli/src/commands/mod.rs` (`Command::Config`, dispatch), `crates/lr-cli/src/commands/reload_cmd.rs` and `logging_cmd.rs` (expose a per-role reload function)
- Test: `crates/lr-cli/tests/config_cmd.rs`, extend `crates/lr-cli/tests/cli_surface.rs`

**Interfaces:**
- CLI: `localrunner config set <target> <key> <value> [--string] [--dry-run] [--restart]`, `config unset <target> <key> [--dry-run] [--restart]`, `config get <target> <key>`, `config show <target>`; `<target>` = `broker | hub | runner:<name> | logging`.
- Produces: `pub async fn run_change(layout: &Layout, target: &Target, change: Change, opts: ApplyOpts, elevator: &dyn Elevator) -> Result<i32, String>` where `ApplyOpts { dry_run: bool, restart: bool, elevated_child: bool }`. Flow: print path and diff and the `widens` lines; `dry_run` ends here with exit 0; if `needs_rights` and not elevated → `elevator.run_elevated`, return its code; else `write`, reload the affected role(s) (policy/runner/hub/logging channel), print each role's answer; for keys in the restart-only table (`runner.allow_elevated`; broker `listen`, `networks`, `shared` until Task 9–10 make them live) print the value written, "applies at the next start" and the restart command (`os_service/state.rs`); `restart` runs it.
- The restart-only table is one const slice `RESTART_ONLY: &[(Target kind, &str key prefix, &str reason)]` in `apply.rs`.

- [ ] **Step 1: Failing tests** over temp directories and fake roles (`tests/common/fake_roles.rs`): `set_writes_file_and_reloads_running_role`, `dry_run_writes_nothing_and_needs_no_rights`, `not_running_role_is_reported_and_exit_zero`, `allow_elevated_prints_widens_line_and_next_start_note`, `restart_only_key_names_the_command_and_does_not_restart_without_flag`, `config_get_and_show`, `unset_missing_key_exit_nonzero`, `needs_rights_goes_through_the_elevator` (fake), `cli_surface_lists_config`.
- [ ] **Step 2–4:** run (FAIL), implement, run (PASS), also `rust-clippy`.
- [ ] **Step 5: Commit** `feat(config): config set|get|unset|show with reload (#136)`.

### Task 6: Policy list verbs

**Files:**
- Create: `crates/lr-cli/src/config_edit/records.rs`, `crates/lr-cli/src/commands/policy_edit_cmd.rs`
- Modify: `args.rs` (`PolicyAction` gets the verbs), `commands/mod.rs`
- Test: `crates/lr-cli/tests/config_records.rs`

**Interfaces:**
- CLI (broker policy): `policy allow-secret <name> --label <label>`, `policy remove-secret <name> --label <label>`, `allow-service`/`remove-service` likewise, `policy add-local-user <os-user> [--label <label>]`, `remove-local-user <os-user>`, `policy add-shared <member>...`, `remove-shared <member>...`. All take `--dry-run`, `--restart`.
- Produces in `records.rs`: `add_name(doc, "secrets.allow", label, name) -> Result<bool, String>`, `remove_name(...) -> Result<bool, String>`, `add_local_user`, `remove_local_user`, `add_shared(doc, members) -> bool`, `remove_shared`. Shape: `[[secrets.allow]]` and `[[services.allow]]` are `{ label, names = [..] }` and the first entry of a label decides, so add appends `name` to the entry of that label (creates the entry when absent) and removes the entry when `names` becomes empty; `[[local_user]]` is `{ name, label }` identified by `name`; `[[shared]]` is `{ members = [..] }` identified by the member set.
- Repeated add: no change, prints "already present", exit 0. Remove of nothing: error, exit 1. Empty label: allowed, prints the same warning `policy reload` prints for an empty label with secrets.

- [ ] **Step 1: Failing tests**: `allow_secret_appends_name_to_label_entry`, `allow_secret_creates_entry`, `allow_secret_twice_is_noop`, `remove_secret_drops_empty_entry`, `remove_missing_is_error`, `empty_label_warns`, `add_local_user_keeps_comments`, `add_shared_identifies_by_member_set`, plus the same for services.
- [ ] **Step 2–4:** run (FAIL), implement over `ConfigDoc` array-of-tables, run (PASS).
- [ ] **Step 5: Commit** `feat(policy): allow-secret, allow-service, add-local-user, add-shared and remove verbs (#136)`.

### Task 7: Hub and runner verbs and `logging`

**Files:** Modify `commands/args.rs`, `commands/mod.rs`; Test `crates/lr-cli/tests/config_cmd.rs`.

- [ ] **Step 1:** Failing tests that `config set hub ...` edits `hub-policy.toml`, `config set runner:<name> cpu 4` edits the own copy (and says when it edits the template), `config set logging <key> <value>` edits `logging.toml` and reloads every role through `logging_cmd::reload_all`.
- [ ] **Step 2–4:** run, wire the targets already supported by `Target`, run.
- [ ] **Step 5: Commit** `feat(config): hub, runner and logging targets (#136)`.

### Task 8: Live apply of [networks] and [[shared]]

**Files:**
- Modify: `crates/lr-broker/src/policy/reload.rs` (`RestartOnly` variants `Networks`, `Shared` stop being reported; keep the type for `Listen` until Task 9), `crates/lr-broker/src/engine/admin.rs` (after `self.policy.reload`, rebuild `self.site`: `Site { broker_addrs, networks: file.networks.clone(), shared: file.shared_sets() }`), `crates/lr-broker/src/engine/mod.rs` (extract the `Site` construction from `Broker::new` into `fn site_of(file: &PolicyFile) -> Site`)
- Test: `crates/lr-broker/tests/policy_reload.rs`, `crates/lr-broker/tests/same_machine.rs`

- [ ] **Step 1: Failing tests**: `reload_applies_networks_to_new_connections` (the hello of a client after reload gets the zone of the new network; an existing connection keeps its zone), `reload_applies_shared_to_new_orders` (an order after reload sees the new shared set; running jobs are untouched), `reload_no_longer_reports_networks_and_shared_as_restart_required`.
- [ ] **Step 2–4:** run `rust-test` for `lr-broker` (FAIL); implement; run (PASS). Update the `RESTART_ONLY` table in Task 5's `apply.rs` and the doc comments of `PolicyFile.networks` and `.shared`.
- [ ] **Step 5: Commit** `feat(broker): apply [networks] and [[shared]] on policy reload (#136)`.

### Task 9: Live apply of [listen]

**Files:**
- Modify: `crates/lr-cli/src/build.rs` (the broker assembly keeps the `MergedListener` and the `TlsListening` of the TLS side), `crates/lr-cli/src/net.rs` (`MergedListener::remove`/swap), the TLS listener type in `lr-net`, `crates/lr-cli/src/broker_admin.rs` (the admin handler calls the swap after a successful reload), `crates/lr-broker/src/policy/reload.rs`
- Test: `crates/lr-cli/tests/listen_swap.rs`

**Interfaces:**
- Produces: `TlsListening::rebind(&self, addresses: Vec<SocketAddr>) -> Result<(), String>`: bind the new sockets first, start accepting on them, then stop accepting on the old ones; established connections are not closed. On bind failure nothing changes and the reload answer carries the error.
- Constraint: the broker's IPC address and the address clients discover stay stable (the recent "keep the broker socket address" change). If the code shows a TLS listener cannot be rebound without dropping established connections, or the discovery address cannot stay stable, keep `[listen]` restart-only: drop this task's code, keep `RestartOnly::Listen`, and record the reason in the docs table (Task 12). Decide from the code before writing tests and say which way you went.

- [ ] **Step 1: Failing tests**: `listen_swap_accepts_on_new_address_and_keeps_old_connection` (loopback addresses; synchronize on the accept event and a message round-trip, no sleeps), `listen_swap_failure_keeps_the_old_listener`, `listen_swap_does_not_drop_a_running_job`.
- [ ] **Step 2–4:** run (FAIL), implement, run (PASS), `rust-clippy`.
- [ ] **Step 5: Commit** `feat(broker): apply [listen] without a restart (#136)`.

### Task 10: rejection messages and label documentation

**Files:** Modify `crates/lr-broker/src/engine/allow.rs`, `crates/lr-broker/src/policy/file.rs` (path of the policy file known to the engine: add `policy_path: String` to `BrokerDeps` or read it from the admin handler), `crates/lr-proto/tests/reasons.rs` if wording is pinned; Test `crates/lr-broker/tests/secrets.rs`, `services.rs`.

- [ ] **Step 1: Failing tests**: the message of `rejected_secret_not_allowed` and `rejected_service_not_allowed` contains the policy file path, the label (`label (empty)` when empty) and the command that fixes it (`localrunner policy allow-secret <name> --label <label>`; mentions "run it as administrator" when the file's directory is the service root), and `empty_label_matches...` documents by test what an empty `label = ""` matches today (read `secrets_allowed` and `service_allowed` in `file.rs` first and pin the real behavior).
- [ ] **Step 2–4:** run (FAIL), implement, run (PASS).
- [ ] **Step 5: Commit** `fix(broker): name the file, the label and the fix in not-allowed rejections (#136)`.

### Task 11: Plugin skills and OS documents

**Files:** Modify `plugin/skills/repo-setup/SKILL.md`, `plugin/skills/repo-setup/reference.md`, `plugin/skills/up-runner/SKILL.md`, `plugin/skills/install-runner/SKILL.md`, `plugin/shared/os/linux.md`, `macos.md`, `windows.md`; Test `crates/lr-cli/tests/skills_lint.rs`.

- [ ] **Step 1:** `rg -n "runner.toml|policy.toml|hub-policy.toml|logging.toml" plugin` and list every place that shows or edits a file by hand; add a failing lint case in `skills_lint.rs` that fails when a skill tells the reader to edit one of the four files by hand (allow reading, allow mentioning the file name in a description).
- [ ] **Step 2:** Run the lint (FAIL). **Step 3:** Replace each hand-edit instruction with the matching `localrunner config ...` or `localrunner policy ...` command; keep the rule that skills do not propose `allow_elevated` and `allow_same_user_as_broker`; describe `--dry-run`, the elevation prompt and `--restart`.
- [ ] **Step 4:** Run `skills-lint` profile (PASS). **Step 5: Commit** `docs(plugin): skills change settings through the CLI (#136)`.

### Task 12: Architecture documents, READMEs and the gate

**Files:** Modify `docs/architecture.html`, `docs/architecture.en.html`, `README.md`, `README.ru.md`.

- [ ] **Step 1:** Add to both HTML files a section "Changing settings through the CLI": the engine flow (Mermaid diagram), the commands, elevation, the `--restart` rule, and the table per file of live keys and restart-only keys with the reason for each restart-only key (at least `runner.toml allow_elevated`). Add the matching commands to both READMEs. Keep each pair in sync.
- [ ] **Step 2:** Run `ci-docs`; expect PASS.
- [ ] **Step 3:** On the final commit run `rust-fmt`, `rust-clippy`, `rust-unit`, `skills-lint` (and `ci-docs`) through `localrunner`; all must pass. If the runner's limits refuse `rust-unit`, run `rust-test` per workspace member and say so in the PR.
- [ ] **Step 4: Commit** `docs: architecture and READMEs for settings through the CLI (#136)`.
- [ ] **Step 5:** Stop. Ask the owner for local acceptance; push and open the PR only after "accepted" (English title and description, no attribution).
