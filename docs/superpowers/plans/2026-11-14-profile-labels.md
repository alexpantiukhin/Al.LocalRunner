# Profile labels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A profile carries free-form `labels` that policy rules read through a new `label` attribute; labels never take part in choosing a runner.

**Architecture:** `Profile.labels` is parsed at profile level and copied into `OrderSpec.labels` by the hub (serde default, skipped when empty, so older senders and stored orders stay valid). The broker copies it into `Declared.labels` next to `source`, `project` and `label`; `StrAttr::Label` matches when any label of the order matches. It is a declared attribute, so the existing trust rule requires a positive `client_label` in a rule that sets a priority by it.

**Tech Stack:** Rust workspace (`lr-core`, `lr-proto`, `lr-hub`, `lr-broker`, `lr-cli`).

**Spec:** issue #88.

## Global Constraints

- `labels` is not a requirement: it is absent from `Requirements`, never read by matching or scheduling.
- Same name rules as tags; an empty list is allowed; duplicates collapse.
- Wire change is additive only: `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
- Docs change in pairs: `docs/architecture.html` + `docs/architecture.en.html`, `README.md` + `README.ru.md`.
- No comments in code except doc comments on public items; run cargo through `localrunner` profiles.

## Review Focus

- A rule `label = "work"` without `client_label` is rejected by `policy check` (trust rule).
- An order with labels runs on a runner that declares none.
- A profile with no `labels` and an old stored order deserialize unchanged.
- `label = { in = [...] }`, `glob`, `not` and `any` work like for `tag`.
- `profile check` and `--json` show the labels.

---

### Task 1: Profile field and parsing (lr-core)

**Files:** `crates/lr-core/src/profile.rs`, `profile_parse.rs`; test `crates/lr-core/tests/profile_labels.rs`.

**Interfaces:** Produces `Profile.labels: Vec<String>` (sorted, unique).

- [ ] Test: `labels = ["work", "release", "work"]` parses to `["release", "work"]`; absent field gives empty; an invalid name fails with the tag error style.
- [ ] Add the field to the raw profile, `Profile` and the default; validate like tags.

### Task 2: Carry labels in the order (lr-proto, lr-hub)

**Files:** `crates/lr-proto/src/job.rs`, `crates/lr-hub/src/orders.rs`; test in `lr-proto` serde round trip.

**Interfaces:** Produces `OrderSpec.labels: Vec<String>`.

- [ ] Test: an `OrderSpec` JSON without `labels` deserializes to empty; one with labels round-trips; empty is not serialized.
- [ ] Add the field; the hub fills it from the resolved profile.

### Task 3: Policy attribute (lr-core, lr-broker)

**Files:** `crates/lr-core/src/queue.rs` (`Declared.labels`), `condition.rs` (`StrAttr::Label`, name `label`, not trusted, values = labels), broker `engine/orders.rs` and `recovery.rs` (copy from spec), `policy/when.rs` (parse key `label`), `explain.rs`/`validate.rs` as needed; tests `crates/lr-core/tests/condition.rs`, `crates/lr-broker/tests/` policy acceptance.

**Interfaces:** Consumes `OrderSpec.labels`. Produces `StrAttr::Label`.

- [ ] Tests: `label = "work"` matches an order with `["work","x"]` and not one with none; `in`, `glob`, `not` forms; `declared_names()` contains `label`; `policy check` rejects `{ label = "work" }` with priority and accepts it with `client_label`; labels do not change runner choice.
- [ ] Implement; first matching rule still wins; `explain` names the rule.

### Task 4: profile check and docs (lr-cli, docs)

**Files:** `crates/lr-cli/src/commands/profile_summary.rs` (`labels` in `ProfileSummary` and a line `  labels: a, b`), docs pairs, `plugin/skills/repo-setup/reference.md` (one line), tests `crates/lr-cli/tests/profile_summary.rs`.

- [ ] Test: summary shows the labels line and the JSON has `labels`.
- [ ] Implement; document the field, the `label` attribute and the trust rule in both architecture files and both READMEs.
