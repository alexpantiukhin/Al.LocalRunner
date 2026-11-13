# Policy Conditions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the broker owner write generic conditions (`any`, `not`, `in`, prefix/glob, comparisons) over order and client attributes in `[[rule]] when`, remove the `[queue] age_after_min` aging, and keep "highest priority next, equal priority oldest, never preempt".

**Architecture:** `Condition` (lr-core) keeps its eight flat equality fields and gains `terms` (typed attribute tests), `any` (OR of sub-conditions) and `not` (negated sub-condition), so the old flat form and every existing struct literal keep compiling and working. The TOML loader (`lr-broker/src/policy`) parses the structured tables into that tree; the trust rule, the shadow warning and `policy check` output are computed over the tree. Aging is deleted from `RulePolicy`; the key `age_after_min` stays accepted as a tolerated, ignored field that produces a warning.

**Tech Stack:** Rust workspace (crates `lr-core`, `lr-broker`, `lr-proto`, `lr-cli`), `serde` + `toml` 0.8 (`deny_unknown_fields`, `Spanned`), no new dependencies (the glob matcher is a small own function).

**Spec:** issue #68 and the decisions below.

**Decisions (approved, do not reopen):**
1. Conditions stay structured TOML tables; no expression string. Old flat `when = { client_label = "ci" }` works unchanged.
2. Kept: no preemption; highest priority goes next; equal priority takes the oldest by the broker's acceptance time (the broker overwrites `Order.enqueued_at` with its own clock at intake, `engine/orders.rs`, and `fifo_key` sorts by it); trust rule (a condition on a claimed, client-declared attribute together with `priority` needs a condition on the trusted `client_label`, else the file is rejected on load); first matching rule that sets a field wins; warning for a rule that can never fire; `policy check` validates and explains the new forms.
3. Aging is removed (`RulePolicy.age_after_min`, `effective_priority`, `max_priority`, `DEFAULT_AGE_AFTER_MIN`, the `[queue]` default, fixtures, tests, docs). A file that still has `[queue] age_after_min` loads, and `check` emits the warning `[queue] age_after_min is no longer used`. Accepted and documented risk: under a constant stream of higher-priority orders a low-priority order can wait indefinitely.
4. Negative priority stays allowed (`i64`) and is documented; only a pinning test, no code change.

**TOML shape (fixed by this plan):**
- String attributes: `client_label`, `client_key`, `machine_id` (trusted); `source`, `project`, `submitter_label`, `platform`, `tag` (declared). Value is a string (equality, as today) or a table with one or more of `eq`, `ne`, `in` (non-empty array of strings), `prefix`, `glob` (`*` any run, `?` one char, case-sensitive); keys inside one table are ANDed.
- Numeric attributes, all declared: `mem_mb`, `cpu`, `disk_mb`, `timeout_s`, and the legacy `min_mem_mb` (kept, means `mem_mb >= n`). Value is an integer (equality) or a table with `eq`, `ne`, `in` (non-empty array), `lt`, `le`, `gt`, `ge`; keys ANDed, so a range is `mem_mb = { ge = 1024, lt = 8192 }`.
- `any = [ {..}, {..} ]` (non-empty array of conditions, at least one must match), `not = { .. }` (non-empty condition, must not match). Everything in one table is ANDed. There is no `all` key: AND is the table itself.
- `tag` is multi-valued: a test passes when some tag of the order (`tags` plus `exclusive`) passes it, except `ne`, which means no tag equals the value.
- A missing attribute value (no project, no platform for a same-machine order) fails `eq`, `in`, `prefix`, `glob` and passes `ne`.
- Not offered: `zone` (a client-to-runner relation, no runner exists when priority is decided) and time windows.

## Global Constraints

- All GitHub texts (commits, PR) in English; no `Co-Authored-By`, no "Generated with"; no agent attribution anywhere.
- No code comments except doc comments (`///`) on public items.
- Every source file stays under 500 lines (`crates/lr-broker/src/policy/sections.rs` is 494 now; aging removal shrinks it; do not grow it). Put new code in the new files named below.
- Generic core: nothing in lr-core, the broker or the scheduler names a specific consumer, language, toolchain or test framework. `ci` appears only as the existing default-rule data in `rules.rs`.
- Docs pairs change together in the same commit: `docs/architecture.html` + `docs/architecture.en.html`, `README.md` + `README.ru.md`.
- `deny_unknown_fields` stays on every policy table; errors carry table, key and line (`PolicyError::at`).
- Run only per-crate cargo commands (`cargo test -p <crate> --test <name>`); do not run the whole workspace.
- Wait reasons and `explain` texts stay as they are (Russian); this plan adds no new wait reason.

## Review Focus

- `any = []`, `not = {}`, `in = []` and an empty comparison table (`mem_mb = {}`) are rejected on load with table, key and line, not silently "match everyone/no one". Pinned in Task 3.
- Negative or fractional numbers (`mem_mb = -1`, `cpu = 1.5`) are rejected on load, not wrapped. Pinned in Task 3.
- A claimed attribute hidden under `any` or `not` (or only in `terms`) with `priority` and no trusted guard is rejected; `not = { client_label = "x" }` and `client_label = { ne = "x" }` are not guards. Pinned in Task 2 and Task 3.
- A rule using the new forms is never reported "never applied" because of an earlier rule it is not covered by; the warning stays conservative. Pinned in Task 5.
- A policy file that still has `age_after_min` reloads, applies, and warns; it never fails with "unknown field". Pinned in Task 4.
- Missing attribute values (order without project) with `ne`/`not`: a rule `project = { ne = "x" }` matches an order that has no project. Pinned in Task 1.

## File Structure

- Create `crates/lr-core/src/condition.rs`: `Condition`, `Term`, attribute and test enums, matching, trust helpers, `covers`, `describe`, `glob_match` (moved out of `policy.rs`, which is 412 lines).
- Modify `crates/lr-core/src/policy.rs`: drop `Condition`, aging; `crates/lr-core/src/lib.rs`: exports; `crates/lr-core/src/strategy.rs`: use `priority_of`.
- Create `crates/lr-broker/src/policy/when.rs`: raw TOML shape of `when` and its conversion to `Condition`.
- Modify `crates/lr-broker/src/policy/rules.rs` (use `when.rs`, tree trust check), `sections.rs` and `mod.rs` (aging), `validate.rs` (warnings), create `crates/lr-broker/src/policy/explain.rs` (rule descriptions for `policy check`).
- Modify `crates/lr-proto/src/ipc.rs`, `crates/lr-cli/src/broker_admin.rs`, `crates/lr-cli/src/commands/admin_cmd.rs` (explain lines in `policy check`).
- Tests: create `crates/lr-core/tests/condition.rs`, `crates/lr-broker/tests/policy_when.rs`, `crates/lr-broker/tests/priority_conditions.rs`; modify the existing tests listed per task.

---

### Task 1: Condition tree and matching in lr-core

**Files:**
- Create: `crates/lr-core/src/condition.rs`
- Modify: `crates/lr-core/src/policy.rs` (delete `Condition` and its impl, lines 57-107; add `use crate::condition::Condition;`), `crates/lr-core/src/lib.rs` (`mod condition;` and re-export `Condition, Term, StrAttr, StrTest, NumAttr, NumTest, glob_match`)
- Test: `crates/lr-core/tests/condition.rs`

**Interfaces:**
- Consumes: `Order`, `ClientView` from `crate::queue` (fields: `order.declared.{source,project,label}`, `order.requirements.{platform,mem_mb,cpu,disk_mb,timeout_s,tags,exclusive}`, `client.{label,key,machine_id}`).
- Produces (all `pub`, doc comments on each; derive `Debug, Clone, PartialEq, Eq`; `StrAttr` and `NumAttr` also `Copy` (their methods take `self` and are called through `&Term` patterns); `Condition` also `Default`):

```rust
pub enum StrAttr { ClientLabel, ClientKey, MachineId, Source, Project, SubmitterLabel, Platform, Tag }
pub enum NumAttr { MemMb, Cpu, DiskMb, TimeoutS }
pub enum StrTest { Eq(String), Ne(String), In(Vec<String>), Prefix(String), Glob(String) }
pub enum NumTest { Eq(u64), Ne(u64), In(Vec<u64>), Lt(u64), Le(u64), Gt(u64), Ge(u64) }
pub enum Term { Str(StrAttr, StrTest), Num(NumAttr, NumTest) }
pub struct Condition {
    pub client_label: Option<String>, pub client_key: Option<String>, pub machine_id: Option<String>,
    pub source: Option<String>, pub project: Option<String>, pub submitter_label: Option<String>,
    pub tag: Option<String>, pub min_mem_mb: Option<u64>,
    pub terms: Vec<Term>,
    pub any: Vec<Condition>,
    pub not: Option<Box<Condition>>,
}
impl Condition { pub fn matches(&self, order: &Order, client: &ClientView) -> bool; }
pub fn glob_match(pattern: &str, text: &str) -> bool;
impl StrAttr { pub fn is_trusted(self) -> bool; pub fn name(self) -> &'static str }
impl NumAttr { pub fn name(self) -> &'static str }
```

Semantics of `matches`: the old eight flat checks (unchanged behaviour) AND every `terms` entry AND (`any` empty or some child matches) AND (`not` is `None` or the child does not match). `StrAttr::is_trusted` is true for `ClientLabel`, `ClientKey`, `MachineId`. `name()` returns the TOML key (`client_label`, ..., `mem_mb`, `cpu`, `disk_mb`, `timeout_s`).

- [ ] **Step 1: Write the failing tests** in `crates/lr-core/tests/condition.rs` (build `Order`/`ClientView` with the helpers in `crates/lr-core/tests/common/mod.rs`; add a small local helper if it lacks a way to set `declared.project` or `requirements.tags`):
  - `flat_form_still_matches_by_equality`: `Condition { client_label: Some("ci".into()), ..Default::default() }` matches a `ci` client, not an `agent` client.
  - `in_matches_any_listed_value`: `Term::Str(StrAttr::ClientLabel, StrTest::In(vec!["ci".into(), "night".into()]))` matches `night`, not `agent`.
  - `prefix_and_glob`: `StrTest::Prefix("lr-")` matches project `lr-core`, not `core-lr`; `StrTest::Glob("o/*-api")` matches `o/x-api`, not `o/x-apis`; `glob_match("a?c", "abc")` true, `glob_match("a?c", "ac")` false, `glob_match("*", "")` true.
  - `numeric_comparisons`: for `mem_mb = 4096`: `Ge(4096)` true, `Gt(4096)` false, `Le(4095)` false, `Lt(4097)` true, `Ne(4096)` false, `In(vec![1024, 4096])` true; same shape checked for `cpu`, `disk_mb`, `timeout_s`.
  - `legacy_min_mem_mb_is_greater_or_equal`: `min_mem_mb: Some(4096)` matches `mem_mb` 4096 and not 4095.
  - `any_not_compose`: `any = [client_label ci, project "x"]` matches either; `not = { project = "x" }` rejects a project `x` order; `any` plus a flat field is an AND of both.
  - `missing_value_fails_positive_tests_and_passes_ne`: an order without `declared.project`: `Eq`, `In`, `Prefix`, `Glob` on `Project` are false, `Ne("x")` is true; same for `Platform` with a `None` platform.
  - `tag_is_multi_valued`: order with `tags={"gpu"}`, `exclusive={"db"}`: `Eq("db")` true, `Prefix("g")` true, `Ne("gpu")` false, `Ne("zzz")` true.
  - `empty_condition_matches_everyone`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lr-core --test condition`
Expected: FAIL to compile (`Term`, `StrTest` not found).

- [ ] **Step 3: Implement** `crates/lr-core/src/condition.rs` with the types above; `glob_match` iterative with backtracking over `char`s (no regex, no recursion on `*`). Remove `Condition` from `policy.rs`; keep `uses_declared` for now as `pub fn uses_declared(&self) -> bool` with its old flat meaning (replaced in Task 2). Re-export from `lib.rs` so every existing `lr_core::Condition` import keeps compiling (`lib.rs:33` currently re-exports `Condition` from `policy`; move it to the `condition` line). Every existing `Condition { .. }` literal in the repo ends in `..Condition::default()` (checked by grep: `rules.rs:30`, tests of lr-core), except `rules.rs:87`, which Task 3 replaces. If `condition.rs` passes about 450 lines, turn it into `condition/mod.rs` plus `condition/glob.rs` and `condition/describe.rs`; the 500-line limit applies.

- [ ] **Step 4: Run to verify pass, and that nothing else in lr-core broke**

Run: `cargo test -p lr-core`
Expected: PASS (the aging tests are still present until Task 4).

- [ ] **Step 5: Commit**

```bash
git add crates/lr-core
git commit -m "feat(core): condition tree with in, prefix, glob, comparisons, any and not"
```

---

### Task 2: Trust rule, coverage and description over the tree

**Files:**
- Modify: `crates/lr-core/src/condition.rs`
- Test: `crates/lr-core/tests/condition.rs` (append), `crates/lr-core/tests/policy.rs:168-169` (the `uses_declared` asserts stay valid)

**Interfaces:**
- Consumes: Task 1 types.
- Produces:

```rust
impl Condition {
    pub fn uses_declared(&self) -> bool;            // now recursive over terms, any, not
    pub fn declared_names(&self) -> Vec<&'static str>; // sorted, deduplicated, recursive; flat `tag` -> "tag", `min_mem_mb` -> "min_mem_mb"
    pub fn guarded_by_label(&self) -> bool;
    pub fn covers(&self, later: &Condition) -> bool;
    pub fn describe(&self) -> String;
}
```

- `guarded_by_label`: true when the top level conjunction has a positive restriction on `client_label`: flat `client_label` is `Some`, or a `Term::Str(ClientLabel, t)` with `t` one of `Eq`, `In`, `Prefix`, `Glob`; or `any` is non-empty and every child is itself guarded. `not` never guards, `Ne` never guards.
- `declared_names`: declared attributes named anywhere in the tree, including under `any` and `not`.
- `covers(&self, later)`: conservative "every order matching `later` also matches `self`". True only if: each set flat field of `self` equals the same field of `later` (`min_mem_mb`: `later` has a value `>=` self's, as today); each `self.terms` entry is equal to some entry of `later.terms`; `self.any` is empty, or `later.any` is non-empty and every child of `later.any` is equal to some child of `self.any` (the later OR-set must be inside the earlier one, not the reverse: earlier `any [A, B]` does not cover later `any [A, B, C]`); `self.not` is `None` or equals `later.not`. Anything else is false (never produces a false "never applied" warning).
- `describe`: `client_label = "ci", project in ["a", "b"], mem_mb >= 8192, any of (...) , not (...)`; an empty condition is `everyone`. Order: the flat fields in struct declaration order (a set `min_mem_mb` prints as `mem_mb >= N`), then `terms` in vector order, then `any`, then `not`. Test spellings: string tests `x = "v"`, `x != "v"`, `x in ["a", "b"]`, `x prefix "v"`, `x glob "v"`; numeric tests `x = 1`, `x != 1`, `x in [1, 2]`, `x < 1`, `x <= 1`, `x > 1`, `x >= 1`. Strings quoted with `{:?}`; parts joined by `, `; `any of (c1 | c2)`; `not (c)`.

- [ ] **Step 1: Write the failing tests**:
  - `declared_inside_any_and_not_is_found`: `not = { project = "x" }` and `any = [{ tag = "gpu" }]` make `uses_declared()` true and `declared_names()` `["project"]` / `["tag"]`; a `Term::Num(MemMb, Ge(1))` gives `["mem_mb"]`.
  - `guard_requires_positive_label_test`: guarded: flat label; `Term::Str(ClientLabel, In(..))`; `any = [{client_label a}, {client_label b}]`. Not guarded: `Ne("x")`; `not = { client_label = "x" }`; `any = [{client_label a}, {project p}]` (one child unguarded); a `client_key` flat field alone; empty condition.
  - `covers_is_conservative`: `{client_label ci}` covers `{client_label ci, project p}`; does not cover `{project p}`; `{any [a,b]}` covers a condition holding an equal `any` and one holding `any [a]`, but not one holding `any [a,b,c]` nor one with no `any`; `{mem_mb Ge(1024)}` (term) covers an identical term only, not `Ge(4096)`.
  - `describe_names_every_part`: exact strings for `{client_label ci}` -> `client_label = "ci"`, an empty condition -> `everyone`, a term/any/not mix.

- [ ] **Step 2: Run** `cargo test -p lr-core --test condition` Expected: FAIL (methods missing).

- [ ] **Step 3: Implement** the methods in `condition.rs`; shape fixed above, bodies are the signature's job.

- [ ] **Step 4: Run** `cargo test -p lr-core` Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lr-core
git commit -m "feat(core): trust guard, coverage and description over the condition tree"
```

---

### Task 3: Parse the new `when` forms and enforce the trust rule on load

**Files:**
- Create: `crates/lr-broker/src/policy/when.rs`
- Modify: `crates/lr-broker/src/policy/rules.rs` (remove `RawCondition`, `declared_fields`; `RawRule.when: RawWhen`; trust check uses `Condition::guarded_by_label` / `declared_names`), `crates/lr-broker/src/policy/mod.rs` (`mod when;`)
- Test: `crates/lr-broker/tests/policy_when.rs`; adjust `crates/lr-broker/tests/policy_errors.rs` only if a message it asserts changes

**Interfaces:**
- Consumes: `lr_core::{Condition, Term, StrAttr, StrTest, NumAttr, NumTest}`; `PolicyError::at(table, key, line, message)`.
- Produces in `when.rs`:

```rust
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWhen { /* client_label, client_key, machine_id, source, project, submitter_label, platform, tag: Option<StrSpec>;
                              min_mem_mb: Option<u64>; mem_mb, cpu, disk_mb, timeout_s: Option<NumSpec>;
                              any: Option<Vec<RawWhen>>, not: Option<Box<RawWhen>> */ }
impl RawWhen { pub(crate) fn into_condition(self) -> Result<Condition, String>; }
```

`StrSpec` and `NumSpec` are enums of the plain value (`String` / `u64`) or a table (`eq`, `ne`, `in`, `prefix`, `glob`, resp. `eq`, `ne`, `in`, `lt`, `le`, `gt`, `ge`). Do NOT use `#[serde(untagged)]`: it buffers the value, the `toml` error loses its span ("data did not match any variant", no table, key or line). Implement `Deserialize` by hand with a visitor (`visit_str`, `visit_i64`/`visit_u64`, `visit_map` into a `deny_unknown_fields` struct; `in` needs `#[serde(rename = "in")]`); a wrong type fails with a message listing the accepted forms (`expected a string or a table with eq, ne, in, prefix, glob`; numbers: `expected a non-negative integer or a table with eq, ne, in, lt, le, gt, ge`). Conversion rules: a plain `client_label`, `client_key`, `machine_id`, `source`, `project`, `submitter_label`, `tag` string fills the existing flat field (so old files produce byte-identical `Condition`s); everything else becomes a `Term`. `into_condition` returns `Err(message)` for: `any = []`, an `any` child that converts to an empty condition (it would match everyone), `not` that converts to an empty condition, `in = []`, a table with no key. `rules::build` turns `Err` into `PolicyError::at("rule", Some("when"), line, "rule {shown}: {message}")`. Trust check in `build`: when `rule.when.uses_declared() && !rule.when.guarded_by_label() && has_action(&rule)` reject with the existing wording using `declared_names().join(", ")`.

- [ ] **Step 1: Write the failing tests** in `policy_when.rs` (use `common::{ok, err}`):
  - `flat_form_unchanged`: `when = { client_label = "ci", source = "github" }` loads with `when.client_label == Some("ci")`, `when.source == Some("github")`, `terms`, `any` empty, `not` `None`.
  - `in_prefix_glob_parse`: `when = { client_label = "ci", project = { in = ["a", "b"] }, submitter_label = { prefix = "claude:" }, source = { glob = "git*" } }` produces the expected `Term::Str(..)` values.
  - `numeric_tables_parse`: `mem_mb = { ge = 1024, lt = 8192 }` gives two `Term::Num(MemMb, ..)`; `cpu = 4` gives `Term::Num(Cpu, NumTest::Eq(4))`; `min_mem_mb = 2048` still fills the flat field.
  - `any_and_not_parse`: nested `any = [{ client_label = "ci" }, { client_label = "night" }]` and `not = { client_label = "x" }` build the tree.
  - `empty_forms_rejected`: each of `any = []`, `any = [{}]`, `not = {}`, `project = { in = [] }`, `mem_mb = {}` fails with `table == "rule"`, `key == Some("when")` and the rule's line.
  - `bad_values_rejected`: `mem_mb = -1`, `cpu = 1.5`, `project = { unknown = "x" }`, `project = 5`, `when = { zone = "lan" }` all fail, each error has `table` and `line`, the unknown-key one names `unknown`, the `project = 5` one lists the accepted forms (`prefix`).
  - `trust_rule_over_the_tree`: rejected (error mentions `client_label`): `when = { project = "p" }` + `priority = 1`; `when = { any = [{ client_label = "ci" }, { project = "p" }] }` + priority; `when = { not = { project = "p" } }` + priority; `when = { mem_mb = { ge = 1 } }` + priority; `when = { client_label = { ne = "x" }, project = "p" }` + priority. Accepted: `when = { client_label = { in = ["a", "b"] }, mem_mb = { ge = 8192 } }` + priority; `when = { any = [{ client_label = "a", project = "p" }, { client_label = "b" }] }` + priority; the same declared conditions on a rule with no action field (`name` only) load (warning only, as today).
  - `negative_priority_allowed`: `priority = -5` loads as `Some(-5)`, and a policy of two rules (`priority = -5` for label `bulk`, nothing else) orders a `bulk` order after a no-rule order (use `rule_policy` + `Policy::order` as in `policy_rules.rs`).

- [ ] **Step 2: Run** `cargo test -p lr-broker --test policy_when` Expected: FAIL.

- [ ] **Step 3: Implement** `when.rs` and the `rules.rs` changes as specified; `default_rules()` is unchanged apart from the extra `Condition` fields coming from `..Condition::default()`.

- [ ] **Step 4: Run**

Run: `cargo test -p lr-broker --test policy_when --test policy_rules --test policy_errors --test policy_defaults --test policy`
Expected: PASS (`policy.rs`/`policy_defaults.rs` still reference `age_after_min`; they are fixed in Task 4, so run them again there).

- [ ] **Step 5: Commit**

```bash
git add crates/lr-broker
git commit -m "feat(broker): parse any, not, in, prefix, glob and comparisons in rule conditions"
```

---

### Task 4: Remove aging, tolerate the key, warn

**Files:**
- Modify: `crates/lr-core/src/policy.rs` (delete `age_after_min` field, `DEFAULT_AGE_AFTER_MIN`, `max_priority`, `effective_priority`; update `with_pool` doc), `crates/lr-core/src/lib.rs` (drop `DEFAULT_AGE_AFTER_MIN`), `crates/lr-core/src/strategy.rs` (`policy.priority_of(e)` instead of `effective_priority`; no parameter becomes unused, `now` still feeds `interleave`, so keep every signature; fix the doc comment "aging included" at `strategy.rs:12` and `policy.rs:146`), `crates/lr-core/src/queue.rs:45,74` (doc comments mention aging), `crates/lr-broker/src/policy/sections.rs` (`QueueSection.age_after_min: Option<u64>`, default `None`, doc: tolerated and ignored; the section doc "ordering strategy and aging" becomes "ordering strategy"; remove the `DEFAULT_AGE_AFTER_MIN` import; the file must not end longer than its 494 lines), `crates/lr-broker/src/policy/mod.rs` (drop `age_after_min:` in `rule_policy`), `crates/lr-broker/src/policy/validate.rs` (warning), `crates/lr-broker/tests/fixtures/spec_policy.toml` (delete line 3)
- Test: modify `crates/lr-core/tests/policy.rs` (delete lines asserting `age_after_min`/`DEFAULT_AGE_AFTER_MIN` at 45-46 and the tests `aging_promotes_after_threshold_and_survives_rebroker`, `aging_threshold_follows_age_after_min`, `aging_raises_to_highest_defined_priority_only`; also drop `DEFAULT_AGE_AFTER_MIN` from its imports at line 6; keep the fifo and strict-priority tests, and add the ones below), `crates/lr-core/tests/strategy.rs:101` (replace `aging_applies_on_top_of_fair_share` by the same queue asserting `["c1", "old"]`: no aging under `fair-share` either), `crates/lr-broker/tests/policy.rs:40,133,136`, `policy_defaults.rs:48-49`, `policy_errors.rs:9`, `policy_reload.rs:72-77,96`, `crates/lr-broker/tests/policy_check.rs`

**Interfaces:**
- Consumes: `PolicyWarning::at(table, key, line, message)`.
- Produces: `QueueSection { strategy: Strategy, age_after_min: Option<u64> }`; `check()` emits `PolicyWarning { table: Some("queue"), key: Some("age_after_min"), line: None, message: "[queue] age_after_min is no longer used" }` when the key is present (an integer; any other type is still a type error as for every key).

- [ ] **Step 1: Write the failing tests**:
  - lr-core `policy.rs`: `a_long_waiting_order_gets_no_priority_boost`: policy with a `ci` rule priority 100, an `agent` order enqueued at 0 and a `ci` order at 10 min, `ordered(.., now = 24 h)` still `["ci", "agent"]`.
  - lr-core `policy.rs`: `equal_priority_takes_the_oldest_by_acceptance_time`: two `agent` orders, the later-listed one has the smaller `enqueued_at`; the result is the smaller first.
  - broker `policy_check.rs`: `age_after_min_loads_and_warns`: `check(&ok("[queue]\nage_after_min = 30\n"), &HOST)` contains a warning with message `[queue] age_after_min is no longer used` and `table == Some("queue")`; `check(&ok(""), &HOST)` has none; the file loads (`ok` does not panic) and `f.queue.age_after_min == Some(30)`.
  - broker `policy_reload.rs`: change the two uses of `age_after_min` to `strategy = "fair-share"` and assert `active.file().queue.strategy == Strategy::FairShare`; add `reload_with_age_after_min_applies_and_warns`: reloading `START` + `[queue]\nage_after_min = 10` returns `Ok` with a warning containing `no longer used` and `settings_update == None`.
  - broker `policy.rs`/`policy_defaults.rs`: replace `age_after_min == 30` asserts by `== None` and remove the `rule_policy(..).age_after_min` assert; `policy_errors.rs:9`: replace the unknown-key value `age_after_min = 5` under `[queu]` by `strategy = "fair-share"` (the test is about the misspelled table).

- [ ] **Step 2: Run** `cargo test -p lr-core --test policy && cargo test -p lr-broker --test policy_check --test policy_reload` Expected: FAIL.

- [ ] **Step 3: Implement** the removals and the warning (in `validate::check`, one `if file.queue.age_after_min.is_some()` block next to the other section warnings).

- [ ] **Step 4: Run**

Run: `cargo test -p lr-core && cargo test -p lr-broker --test policy --test policy_defaults --test policy_errors --test policy_reload --test policy_check --test policy_rules --test policy_when`
Expected: PASS. Then `rg age_after_min crates` must list only the tolerated field, the warning, and their tests.

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat(policy): remove priority aging; age_after_min is tolerated and warns"
```

---

### Task 5: Shadow and never-fires warnings over the tree

**Files:**
- Modify: `crates/lr-broker/src/policy/validate.rs` (delete `implied_by`, call `Condition::covers`; extend the pool-memory warning)
- Test: `crates/lr-broker/tests/policy_check.rs` (append; the five existing shadow tests must still pass untouched)

**Interfaces:**
- Consumes: `Condition::covers(&self, later) -> bool` (Task 2).
- Produces: warnings keep their messages (`rule X is never applied: earlier rule Y decides every field it sets`). The pool check also covers numeric terms: a top-level `Term::Num(MemMb, Eq(n) | Ge(n))` with `n > pool.mem_mb`, or `Gt(n)` with `n >= pool.mem_mb` (an order of exactly the pool size is accepted), or `min_mem_mb` above the pool yields the existing message shape `rule X: min_mem_mb = N is more than the pool's M MB; no order that large is ever accepted`, with `mem_mb >= N` text for terms.

- [ ] **Step 1: Write the failing tests**:
  - `tree_rule_shadowed_by_equal_earlier_rule`: two rules with the identical `when = { client_label = "ci", any = [{ project = "a" }, { project = "b" }] }` and `priority`; the second is reported as never applied.
  - `broader_condition_with_terms_shadows`: earlier `when = { client_label = "ci", mem_mb = { ge = 1024 } }`, later `when = { client_label = "ci", mem_mb = { ge = 1024 }, project = "p" }` -> warned.
  - `different_terms_do_not_shadow`: earlier `mem_mb = { ge = 1024 }`, later `mem_mb = { ge = 4096 }` -> no "never" warning (conservative).
  - `not_and_any_are_not_guessed`: earlier `not = { project = "a" }`, later `project = "b"` -> no "never" warning.
  - `memory_term_above_pool_warned`: HOST has 32768 MB; `mem_mb = { ge = 99999 }` is warned with `is more than the pool`.

- [ ] **Step 2: Run** `cargo test -p lr-broker --test policy_check` Expected: FAIL on the new tests.

- [ ] **Step 3: Implement** as above.

- [ ] **Step 4: Run** `cargo test -p lr-broker --test policy_check` Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lr-broker
git commit -m "feat(policy): shadow and size warnings understand the condition tree"
```

---

### Task 6: `policy check` explains the rules

**Files:**
- Create: `crates/lr-broker/src/policy/explain.rs` (export from `mod.rs`)
- Modify: `crates/lr-proto/src/ipc.rs:345` (`AdminReply::Policy` gains a field), `crates/lr-proto/tests/common/ipc.rs:136` (add the field), `crates/lr-cli/src/broker_admin.rs:210`, `crates/lr-cli/src/commands/admin_cmd.rs` (`broker_policy` at 13-27, `policy_check`, the pattern `AdminReply::Policy { path, warnings }` in `show` at 216, the `Some(Ok((path, _)))` at 135), `crates/lr-cli/src/doctor.rs:125` (the other caller of `broker_policy`)
- Test: `crates/lr-broker/tests/policy_check.rs` (append), `crates/lr-cli/tests/broker_admin.rs` (append), round-trip and old-sender tests for the new field in `crates/lr-proto/tests/compat_reads.rs` (the other proto test files are near or over 500 lines)

**Interfaces:**
- Consumes: `Condition::describe`, `display_name`, `RuleEntry`.
- Produces:

```rust
pub fn explain_rules(file: &PolicyFile) -> Vec<String>;
```

One line per rule, 1-based: `rule 1 ci-first (line 4): when client_label = "ci"; priority 100`. Default (file-less) rule has no `(line N)`. Fields appended in the order `priority`, `reserve cpu/mem_mb`, `quota`, `weight`, only those set. A file with no rules gives an empty vector. `AdminReply::Policy { path: String, warnings: Vec<String>, #[serde(default, skip_serializing_if = "Vec::is_empty")] rules: Vec<String> }`; `broker_policy` returns `Option<Result<BrokerPolicy, String>>` with a new `pub struct BrokerPolicy { pub path: String, pub warnings: Vec<String>, pub rules: Vec<String> }` (a three-tuple risks clippy `type_complexity`); update `doctor.rs`, `admin_cmd.rs:135` and `crates/lr-cli/tests/broker_admin.rs:180,199` to it; both `policy check` paths (running broker and local file) print `rule: <line>` lines after the warnings and before the final `valid` line.

- [ ] **Step 1: Write the failing tests**:
  - `explain_rules_names_every_rule`: for a two-rule file with `name = "ci-first"` (line known) and an unnamed second rule with `when = { client_label = "ci", project = { in = ["a", "b"] } }`, `priority = -1`, the vector equals the expected two strings exactly (`rule 2 (line N): when client_label = "ci", project in ["a", "b"]; priority -1`).
  - `explain_rules_default_rule`: empty file gives `rule 1 ci-first: when client_label = "ci"; priority 100`.
  - proto: an `AdminReply::Policy` without the `rules` key deserializes (old broker) with an empty vector; with rules it round-trips.
  - cli `broker_admin.rs`: extend `policy_check_names_the_file_the_running_broker_uses` style test: the `AdminReply::Policy { rules, .. }` for the default policy contains `rule 1 ci-first`.

- [ ] **Step 2: Run** `cargo test -p lr-broker --test policy_check && cargo test -p lr-proto && cargo test -p lr-cli --test broker_admin` Expected: FAIL.

- [ ] **Step 3: Implement** `explain.rs`, the field, the call at `broker_admin.rs:210` (`rules: explain_rules(&file)`) and the CLI printing.

- [ ] **Step 4: Run** the same commands. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat(cli): policy check explains each rule and its condition"
```

---

### Task 7: Acceptance tests through the broker

**Files:**
- Create: `crates/lr-broker/tests/priority_conditions.rs` (model on `assign.rs:119-160`: `Harness::start(policy_text)`, `member_runner`, `local_hub`, `member_hub`, `order_at`, `req`, `result`, `started`)
- Modify: none in `src/`; if a test fails, fix the matching earlier task, not the test.

**Interfaces:**
- Consumes: the harness in `crates/lr-broker/tests/common/harness.rs`, policy loader, `ActivePolicy::reload` for the reload test (see `policy_reload.rs` `started()` helper).
- Produces: the issue's acceptance tests.

- [ ] **Step 1: Write the tests** (all fail only if earlier tasks are wrong, so this step also runs green once Task 1-4 are done):
  - `condition_in_gives_priority_and_goes_next`: policy `[[rule]] when = { client_label = { in = ["ci", "night"] } } priority = 50`; one runner with a running job from alex; orders `a1` (alex) then `n1` (label `night`) queue up; after the running job finishes the next assigned job is `n1`.
  - `higher_priority_goes_next_without_stopping_the_running_job`: as `assign.rs::running_job_is_never_preempted_by_higher_priority` but with a policy using `when = { client_label = { prefix = "ci" }, mem_mb = { ge = 256 } }`; the `ci-box` order waits, the running job row stays `Running`, the runner receives nothing but the ack.
  - `equal_priority_takes_the_oldest`: policy giving `ci` and `night` both `priority = 50`; both after a running job; the broker overwrites `enqueued_at` with its `ManualClock` (`engine/orders.rs:406`), so call `h.clock.advance(Duration::from_secs(1))` between the two orders (without it the tie falls to the job id) and name the jobs so the id order is the opposite of the arrival order (`z-night` first, `a-ci` second); `z-night` is assigned first. Then the same with the arrival order swapped.
  - `claimed_attribute_without_trusted_condition_is_rejected_on_load`: `load` of `[[rule]] when = { project = { glob = "lr-*" } } priority = 9` fails and the message names `client_label`; the same rule with `client_label = { in = ["ci"] }` added loads.
  - `reload_applies_new_conditions_and_keeps_old_on_error`: `ActivePolicy` started with `[[rule]] when = { client_label = "ci" } priority = 100`, reload with `when = { not = { client_label = "ci" } , client_label = "agent" } priority = 100` (valid) changes `rule_policy(..)` ordering for an `agent` order; reload with `any = []` returns `Err` and the previous rules stay.
  - `negative_priority_orders_last`: rule `client_label = "bulk"` `priority = -10`; a `bulk` order waits behind a later-arriving order from a client without a rule.

- [ ] **Step 2: Run** `cargo test -p lr-broker --test priority_conditions` Expected: PASS (if a test exposes a bug, fix it in the owning task's file and re-run that task's tests).

- [ ] **Step 3: Commit**

```bash
git add crates/lr-broker/tests/priority_conditions.rs
git commit -m "test(broker): acceptance tests for policy conditions and priority order"
```

---

### Task 8: Documentation (both pairs)

**Files:**
- Modify: `docs/architecture.html`, `docs/architecture.en.html`, `README.md`, `README.ru.md`

Same edits in both language files of a pair, in the same commit. Locate by the Russian/English text (line numbers are from the English file):

- [ ] **Step 1: `[queue]` example (line ~281-282 of the policy example):** delete the `age_after_min` line; add after the `ci-reserve` rule a second example rule block showing the new forms, e.g. `when = { client_label = { in = ["ci", "night"] }, project = { glob = "lr-*" }, mem_mb = { ge = 8192 } }` with `priority = 50` and one `any`/`not` example in a comment-free table. Write every inline table on one line (`toml` 0.8 is TOML 1.0: no newlines inside `{ }`). The fixture `crates/lr-broker/tests/fixtures/spec_policy.toml` is the documented example: add the same new rule to it, change `crates/lr-broker/tests/policy.rs:41` from 2 to 3 rules and assert the new rule's condition; `spec_example_rules_are_not_shadowed` must still pass.
- [ ] **Step 2: section 3.5, row "Assignment order, priorities, aging, ..." (line ~271):** drop "aging".
- [ ] **Step 3: "Policy semantics" `Strategies` item (line ~394):** delete the sentence "Aging (`age_after_min`) applies on top of any strategy."
- [ ] **Step 4: "Policy semantics" `The when conditions` item (line ~395):** rewrite: a rule matches when everything in its `when` table matches; list trusted fields (`client_key`, `client_label`, `machine_id`) and claimed fields (`source`, `project`, `submitter_label`, `platform`, `tag`, `mem_mb`, `cpu`, `disk_mb`, `timeout_s`, legacy `min_mem_mb` = `mem_mb >=`); string tests `eq`, `ne`, `in`, `prefix`, `glob` (`*`, `?`); numeric tests `eq`, `ne`, `in`, `lt`, `le`, `gt`, `ge`; `any = [..]` (at least one) and `not = {..}`; a missing value fails every positive test and passes `ne`; `tag` is true if some tag of the order passes; empty `any`, `not`, `in` and empty test tables are rejected. Trust rule: a rule with a claimed field anywhere in `when` (also under `any` or `not`) and any of `priority`, `reserve`, `quota`, `weight` needs a positive condition (`eq`, `in`, `prefix`, `glob`) on `client_label` at the top level or in every `any` branch, otherwise the file is rejected.
- [ ] **Step 5: add one `<li>` to "Policy semantics" on priority:** `priority` is an `i64`, a negative number is allowed and means "after orders without a rule" (0); the order for the next start is highest priority first, equal priority by the broker's acceptance time; no preemption; there is no aging, so a low-priority order can wait indefinitely under a constant stream of higher-priority orders (accepted risk; give such orders a rule or cancel and resubmit when this matters); a policy that still has `[queue] age_after_min` loads and gets the warning `[queue] age_after_min is no longer used`.
- [ ] **Step 6: "Default values" item (line ~396):** `[queue]`: `priority-fifo` only (remove "30 minutes").
- [ ] **Step 7: section 6 (lines 733, 736):** line 733 ("Backfilling"): rewrite "The policy decides the order, and aging (`age_after_min`) moves an order that has waited a long time to the head" to "The policy decides the order" (the ru file has the same sentence); delete the bullet at 736 ("Aging (30 minutes by default...)") and add the one-line indefinite-wait note; line ~385 ("Trusted and claimed attributes"): remove "FIFO and aging are counted from it" to "FIFO is counted from it", and the sentence "so a client cannot 'age' an order" to "so a client cannot jump the queue by claiming an older time".
- [ ] **Step 8: glossary entry `g-priority` (line ~93):** remove "and a job older than 30 minutes is raised to the highest priority"; add "a negative priority is allowed". Line ~659 ("Priority is not derived from characteristics"): add "conditions can use sets, prefixes, patterns, comparisons, any and not". Section 16.14 diagram (line 1300, both files): remove the aging fragment (the `rect` at y=536 with its text "waits longer than age_after_min" / "ждёт дольше age_after_min", and the path and text at y=592/586 "order as for the highest priority"), then cut the `viewBox` height and the `V618` lifeline ends by 84.
- [ ] **Step 9: `README.md` line 217 and `README.ru.md` line 219:** `[queue]` row without `age_after_min`; the `[[rule]]` row mentions the new condition forms in one clause; if the READMEs have a policy section, add a short example identical in both files.
- [ ] **Step 10: Verify**

Run: `rg -in "age_after_min|aging|старени" README.md README.ru.md docs/architecture.html docs/architecture.en.html`
Expected: only the intentional "no longer used" and "no aging" notes remain, the same set in each language of a pair. Open both HTML files in the table of contents check is not needed (no new headings); keep each pair's diff symmetric (`git diff --stat` shows both files of each pair).

- [ ] **Step 11: Commit**

```bash
git add docs README.md README.ru.md
git commit -m "docs: policy conditions, negative priority and removal of aging"
```

---

### Task 9: Final verification (per crate)

- [ ] **Step 1:** `cargo fmt --check -p lr-core -p lr-broker -p lr-proto -p lr-cli` Expected: no diff.
- [ ] **Step 2:** `cargo clippy -p lr-core -p lr-broker -p lr-proto -p lr-cli --all-targets -- -D warnings` Expected: clean.
- [ ] **Step 3:** `cargo test -p lr-core -p lr-broker -p lr-proto` and `cargo test -p lr-cli --test broker_admin --test cli_surface` Expected: PASS.
- [ ] **Step 4:** `wc -l` on every touched `.rs` file: all under 500; `rg "age_after_min|effective_priority|DEFAULT_AGE" crates` shows only the tolerated field, the warning and their tests.
- [ ] **Step 5:** Open the PR with title `feat(policy): universal conditions for launch priority rules` and a body that says `Closes #68`, lists the removal of aging and the new TOML shape; no agent attribution.

## Self-review notes

- Spec coverage: structured forms (Tasks 1-3), unchanged semantics and tests (Task 7), trust rule over the tree (Tasks 2-3), aging removal with tolerated key and warning through `validate::check`, which feeds `policy check`, `policy reload` and the broker start log (Task 4), shadow warning (Task 5), `policy check` explain (Task 6), negative priority (Tasks 3, 7, 8), docs (Task 8).
- `explain`/`status` today name only reserve and quota rules in wait reasons (`Wait::Reserved`/`Wait::Quota`); no code names the priority rule, so there is nothing to preserve there and this plan adds nothing; the rule description for priority is `policy check` output only.
