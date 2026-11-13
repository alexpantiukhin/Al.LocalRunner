# Profile arguments (#72) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A profile declares named arguments; `submit --arg name=value` fills them into the profile commands, so one profile serves many test targets.

**Architecture:** Declarations live in `Profile.args` (lr-core, parsed from `[profile.X.args.<name>]`). The hub substitutes values into the step commands when it builds the order (`lr-hub/src/orders.rs`) and records the given pairs in `OrderSpec.args`; broker and runner see an ordinary order with the final command. Resources are not touched: `--mem`/`--cpu` keep working as today.

**Tech Stack:** Rust workspace (lr-core, lr-proto, lr-hub, lr-cli), `serde`, `toml`, `clap`, new `regex` dependency in lr-core.

**Spec:** issue #72 and the design approved in the conversation, written into `docs/architecture.html` / `docs/architecture.en.html` by Task 6.

## Global Constraints

- The core stays generic: no branch on a consumer, language, toolchain or test framework.
- No security validation of values (decision of the owner: runs are local, the submitter can run anything anyway). `one_of`/`pattern` are optional conveniences that catch typos; do not reject newlines, NUL, quotes or shell characters.
- Profiles without `args` behave exactly as before (no template parsing of their commands).
- Unknown `{name}` in a command is left untouched: the runner fills `{repo_url}`, `{commit}`, `{branch}`, `{workspace}`, `{job_dir}` in `source` steps (`crates/lr-runner/src/source.rs`). Those five names are reserved: an argument with such a name is a profile error.
- New wire fields are backward compatible: `#[serde(default, skip_serializing_if = ...)]`, like `mem_mb` of `SubmitRequest` and `overrides` of `OrderSpec`.
- Arguments never influence priority, reserve, quota or admission (they are claimed data); client identity still comes from the OS and the key.
- No code comments except doc comments on public items; files under 500 lines; GitHub texts and commits in English, no agent attribution lines in commits.
- Docs: change `docs/architecture.html` and `docs/architecture.en.html` together; update `README.md` and `README.ru.md` together.

## Review Focus

- A value with spaces, quotes or `$` is substituted verbatim (documented, not an error).
- The same `--arg` name given twice for an argument without `many = true` is refused, not silently last-wins.
- `[ ... ]` markers do not appear: the optional segment is `{? ... }`; a command with a literal `{?` in a profile that has no `args` is untouched.
- An optional segment containing two arguments disappears when either is missing.
- `--arg` for a profile that declares no `args` is refused with the list of declared names (empty).
- A profile from a commit (`--platform`) and from the working tree both get substitution (same code path).

---

### Task 1: Argument declarations in profiles (lr-core)

**Files:**
- Create: `crates/lr-core/src/profile_args.rs` (model; binding is added in Task 2)
- Modify: `crates/lr-core/src/profile.rs` (add `args` to `Profile`), `crates/lr-core/src/profile_parse.rs` (parse `args`), `crates/lr-core/src/lib.rs` (export), `Cargo.toml` and `crates/lr-core/Cargo.toml` (`regex`)
- Test: `crates/lr-core/tests/profile_args.rs`

**Interfaces:**
- Produces:
  - `pub struct ArgSpec { pub name: String, pub one_of: Option<Vec<String>>, pub pattern: Option<String>, pub default: Option<String>, pub many: bool, pub join: String }` (derives `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`; `join` is `" "` unless set).
  - `Profile.args: Vec<ArgSpec>` with `#[serde(default)]`, ordered by name.
  - `pub const RESERVED_ARG_NAMES: [&str; 5]` = `repo_url, commit, branch, workspace, job_dir`.

- [ ] **Step 1: Write failing tests** in `crates/lr-core/tests/profile_args.rs` (use the `one`/`invalid` helper style of `tests/profile.rs`):
  - `args_table_parses`: `[profile.t] command = "cargo test {target}" timeout_s = 60` plus `[profile.t.args.target] one_of = ["all","a"] default = "all"` gives `args == [ArgSpec{name:"target", one_of:Some(["all","a"]), pattern:None, default:Some("all"), many:false, join:" "}]`.
  - `many_and_join_parse`: `many = true, join = ","` → `many == true`, `join == ","`.
  - `profile_without_args_has_none`: existing example profiles parse with `args.is_empty()`.
  - `one_of_and_pattern_together_rejected`, `bad_regex_rejected` (field `args.<name>.pattern`), `default_outside_one_of_rejected`, `reserved_name_rejected` (name `branch`), `unknown_key_in_arg_rejected` (the table uses `deny_unknown_fields`), `join_without_many_rejected`.
- [ ] **Step 2: Run** `cargo test -p lr-core --test profile_args` → FAIL (types missing).
- [ ] **Step 3: Implement** `ArgSpec` in `profile_args.rs`; `RawArg` with `deny_unknown_fields` in `profile_parse.rs` and a field `args: BTreeMap<String, RawArg>` (`#[serde(default)]`) in `RawProfile`; validate through `ProfileError::invalid(&profile, &format!("args.{name}.<key>"), reason)`. Add `regex` as a workspace dependency (default features off, `std` + `unicode` on) and check it with `cargo deny check`. Add `args: Vec<ArgSpec>` (`#[serde(default)]`) to `Profile` and fill it in `build`. Anchor `pattern` as a full match (wrap as `^(?:p)$`).
- [ ] **Step 4: Run** `cargo test -p lr-core` → PASS (all existing profile tests unchanged).
- [ ] **Step 5: Commit** `feat(core): declare named arguments in profiles`.

---

### Task 2: Binding arguments into step commands (lr-core)

**Files:**
- Modify: `crates/lr-core/src/profile_args.rs`, `crates/lr-core/src/error.rs`, `crates/lr-core/src/lib.rs`
- Test: `crates/lr-core/tests/profile_args_bind.rs`

**Interfaces:**
- Consumes: `ArgSpec`, `StepSpec` (Task 1 / existing).
- Produces:
  - `pub fn bind_args(steps: &mut [StepSpec], specs: &[ArgSpec], given: &[(String, String)]) -> Result<(), ProfileError>` — rewrites `StepSpec.command` of every step.
  - `ProfileError::Args(String)` (`#[error("arguments: {0}")]`).
  - Template rules, in code the implementer follows: `{name}` of a declared argument is replaced by its value(s); `{? text }` is an optional segment (text may hold declared placeholders, the closing brace is the matching one counting nested `{...}`), dropped entirely when any argument inside has no value and no default; other `{...}` stay as they are. A step command of a profile whose `specs` is empty is returned untouched (even with `{?`).

- [ ] **Step 1: Write failing tests** (build `StepSpec`s with a small helper; specs from `ArgSpec` literals):
  - `substitutes_value`: `"cargo test {target}"`, `target=a` → `"cargo test a"`.
  - `default_used_when_not_given`: default `all` → `"cargo test all"`.
  - `value_is_verbatim`: value `a b "c" $X` stays exactly (no escaping, no rejection).
  - `optional_segment_dropped_and_kept`: `"cargo test {? -p {pkg}} {target}"`: without `pkg` the result is `"cargo test all"` (the whitespace before a dropped segment is removed, no double space); with `pkg=x` → `"cargo test -p x all"`.
  - `segment_with_two_args_needs_both`.
  - `many_joins_values`: `many`, `join=","`, two values → `"a,b"`; repeated name without `many` → `Err(Args)` whose text names the argument.
  - `missing_required_is_error`: placeholder outside `{? }`, no default, not given → error naming the argument.
  - `unknown_given_name_is_error` listing the declared names (empty list for no specs).
  - `value_must_match_one_of_and_pattern`: error text lists the allowed values / the pattern.
  - `unknown_braces_untouched`: `"git clone {repo_url} ${HOME} {a,b}"` unchanged.
  - `no_specs_leaves_template_alone`.
  - `applies_to_every_step`.
- [ ] **Step 2: Run** `cargo test -p lr-core --test profile_args_bind` → FAIL.
- [ ] **Step 3: Implement** `bind_args` with a single left-to-right scan of the command (no regex for the template; `regex` only for `pattern`), a helper that renders one segment or reports `Missing(name)`.
- [ ] **Step 4: Run** `cargo test -p lr-core` → PASS; `cargo clippy -p lr-core --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** `feat(core): bind submit arguments into step commands`.

---

### Task 3: Wire fields (lr-proto and every literal)

**Files:**
- Modify: `crates/lr-proto/src/ipc.rs` (`SubmitRequest`), `crates/lr-proto/src/job.rs` (`OrderSpec`); every struct literal found by `cargo build --workspace --all-targets`: `SubmitRequest` in `crates/lr-cli/src/commands/jobs.rs`, `crates/lr-cli/tests/{common/loopback.rs,e2e/selection.rs,e2e/support.rs,read_wiring.rs,status_filters.rs,submit_done.rs,wiring.rs}`, `crates/lr-hub/tests/common/mod.rs`; `OrderSpec` in `crates/lr-hub/src/orders.rs`, `crates/lr-broker/tests/common/{mod.rs,msgs.rs}`, `crates/lr-core/tests/common/mod.rs`, `crates/lr-proto/tests/common/mod.rs`
- Test: `crates/lr-proto/tests/compat.rs`

**Interfaces:**
- Produces: `SubmitRequest.args: Vec<(String, String)>` and `OrderSpec.args: Vec<(String, String)>`, both `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, pairs in the order given.

- [ ] **Step 1: Write failing test** next to the `overrides` compat test: an old `SubmitRequest`/`OrderSpec` JSON without `args` deserializes with empty `args`; with `args = [("target","a"),("target","b")]` serialization gives `["target","a"]` pairs and round-trips; empty `args` is absent from the JSON.
- [ ] **Step 2: Run** `cargo test -p lr-proto --test compat` → FAIL.
- [ ] **Step 3: Add the fields**, set `args: Vec::new()` in all literals; in `lr-hub/src/orders.rs` leave `Vec::new()` for now.
- [ ] **Step 4: Run** `cargo test --workspace` → PASS.
- [ ] **Step 5: Commit** `feat(proto): carry submit arguments in SubmitRequest and OrderSpec`.

---

### Task 4: The hub binds arguments (lr-hub)

**Files:**
- Modify: `crates/lr-hub/src/orders.rs` (`build`)
- Test: `crates/lr-hub/tests/submit_args.rs` (new file; `submit.rs` is already long)

**Interfaces:**
- Consumes: `bind_args` (Task 2), `SubmitRequest.args`/`OrderSpec.args` (Task 3).
- Produces: orders whose `steps[].command` are final and `spec.args == req.args`.

- [ ] **Step 1: Write failing tests** with the `Rig` helpers of `tests/common` (see `mem_flag_may_lower_the_profile_value` in `tests/submit.rs`; add a profile with `args` to the rig's profiles file):
  - `arg_fills_the_command_and_is_recorded`: `submit.args = [("target","peer_key_store")]` → the order sent to the broker has `steps[0].command == "cargo test peer_key_store"` and `spec.args == [("target","peer_key_store")]`.
  - `commit_order_gets_substitution_too` (a `platform` order from a commit).
  - `bad_arg_is_malformed_and_nothing_is_sent`: unknown name, value outside `one_of`, missing required → `CliReply::Error` with `ProtocolErrorCode::Malformed` and text naming the argument; broker received no order.
  - `args_do_not_change_requirements`: resources and timeout equal the profile's, and `--mem` still overrides.
  - `profile_without_args_is_unchanged`: an old profile order is byte-equal to today's.
- [ ] **Step 2: Run** `cargo test -p lr-hub --test submit_args` → FAIL.
- [ ] **Step 3: Implement** in `build`: after `resolved` is computed call `bind_args(&mut resolved.steps, &profile.args, &req.args)` mapping the error to `invalid(e.to_string())`, and set `args: req.args.clone()` in the `OrderSpec`. `resolved` must become `mut`.
- [ ] **Step 4: Run** `cargo test -p lr-hub` → PASS; `cargo clippy -p lr-hub --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** `feat(hub): fill profile arguments when building the order`.

---

### Task 5: `--arg` flag (lr-cli)

**Files:**
- Modify: `crates/lr-cli/src/commands/mod.rs` (`OrderArgs`), `crates/lr-cli/src/commands/jobs.rs` (`submit`)
- Test: `crates/lr-cli/tests/submit_args.rs` (new)

**Interfaces:**
- Consumes: `SubmitRequest.args` (Task 3).
- Produces: `OrderArgs.args: Vec<(String, String)>` from a repeatable `--arg name=value` (`value_parser` splitting at the first `=`; an empty name or no `=` is a clap error). Both `submit` and `run` get it (they share `OrderArgs`).

- [ ] **Step 1: Write failing tests**: `Cli::try_parse_from(["localrunner","submit","p","--arg","target=a","--arg","target=b","--arg","x=y=z"])` yields `[("target","a"),("target","b"),("x","y=z")]`; `--arg novalue` and `--arg =v` fail; the same flags parse for `run`.
- [ ] **Step 2: Run** `cargo test -p lr-cli --test submit_args` → FAIL.
- [ ] **Step 3: Implement** the `#[arg(long = "arg", value_name = "NAME=VALUE", value_parser = parse_arg)]` field and pass it into `SubmitRequest { args: args.args, .. }` in `commands/jobs.rs`.
- [ ] **Step 4: Run** `cargo test -p lr-cli` → PASS.
- [ ] **Step 5: Commit** `feat(cli): submit --arg name=value`.

---

### Task 6: Documentation and agent skill

**Files:**
- Modify: `docs/architecture.html` and `docs/architecture.en.html` (section 5 "Profiles and steps": new subsection "5.2 Arguments" after "5.1 Steps"; the `submit --mem` bullet of section 5 mentions `--arg`; glossary entry "Argument"), `README.md` and `README.ru.md` (the line that lists `submit` flags, found by `grep -n -e "--mem" README*.md`), `plugin/skills/run/SKILL.md` (one line telling agents they can pass `--arg name=value` when the profile declares arguments, and that resources still come from `--mem`/`--cpu`)

- [ ] **Step 1: Write the subsection** (both languages, the same content): the `[profile.X.args.<name>]` keys (`one_of`, `pattern`, `default`, `many`, `join`), `{name}` and `{? ... }`, unknown braces untouched and the five reserved names, repeated `--arg` with `many`, errors before the queue, no value sanitizing (local runs; `one_of`/`pattern` only catch typos), resources unchanged, arguments recorded in the order and not used by the policy. Use the `rust-test` example from the issue.
- [ ] **Step 2: Check** that both HTML files differ only in language: `diff <(grep -c "<h" docs/architecture.html) <(grep -c "<h" docs/architecture.en.html)` prints nothing; no file is left over 500 lines except the two HTML documents.
- [ ] **Step 3: Run** `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` → all pass.
- [ ] **Step 4: Commit** `docs: profile arguments`.
- [ ] **Step 5: Push the branch and open the pull request** (English title and body, `Closes #72`, no agent attribution lines); ask the owner before pushing.
