# Submit Without --detach Warning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make it impossible to miss that `submit` without `--detach` cancels the order when the sending process exits: state it outright in the skill and docs, and print one warning line to stderr when `submit` is run without `--detach`, without `--wait` and without a terminal.

**Architecture:** One small public function in `crates/lr-cli/src/commands/jobs.rs` decides and prints the warning; `jobs::submit` calls it after a successful non-waiting submit with the real terminal state. Everything else is text: the `run` skill, `--detach` help, both READMEs and both architecture documents.

**Tech Stack:** Rust (clap, `std::io::IsTerminal`), the repository's cargo test suite, Markdown and HTML docs.

**Spec:** issue #71 and the decisions below.

## Decisions (approved by the owner, not reopened)

1. `plugin/skills/run/SKILL.md` and the docs state outright: without `--detach` the order is cancelled when the sending process exits; an agent session (short-lived shell calls) always passes `--detach` and then waits (`localrunner wait <job-id>`), or uses `run` (`submit --wait`).
2. `submit` without `--detach` and without a terminal prints exactly one warning line to stderr. No warning for `--wait`, for `run`, or with `--detach`. `--json` stdout stays clean (the warning is on stderr; `--json` does not suppress it).
3. "Project docs template" from the issue: the repository has no such template (`git ls-files` shows only `docs/architecture*.html` and `docs/superpowers/plans`); that part is skipped.
4. Terminal detection follows the existing code (`confirm.rs`, `setup.rs`, `join_cmd.rs`): `std::io::stdin().is_terminal()`.
5. Warning text, exactly: `warning: without --detach this order is cancelled when this process exits; pass --detach and then run "localrunner wait <job-id>", or use "localrunner run"`

## Global Constraints

- GitHub texts (commits, PR) in English; no `Co-Authored-By` or "Generated with" lines; PR body has `Closes #71`.
- No code comments except doc comments (`///`) on public items.
- Files stay under 500 lines (`jobs.rs` is 454 now; this plan adds about 15).
- Docs pairs change together in the same commit: `README.md` + `README.ru.md`, `docs/architecture.html` + `docs/architecture.en.html`.
- Generic core: nothing names a consumer or toolchain.
- The warning is written to stderr only; stdout carries the JobId (or the `--json` line) alone.

## Review Focus

- `submit --wait` and `run` print no warning (the process lives until the end): covered by Task 1 test `no_warning_with_wait`.
- `submit --detach` prints no warning: `no_warning_with_detach`.
- A terminal session prints no warning: `no_warning_with_a_terminal`.
- `submit --json` without `--detach` keeps stdout exactly `{"job":...}`; the warning goes to stderr only: `submit_on` is untouched and `warn_if_orphaned` takes only the stderr writer (its test writes to a `Vec<u8>`).
- A rejected or failed submit prints no warning: the call sits after a successful `Ok(0)` result (Task 1, step 3).

---

### Task 1: The warning in `submit`

**Files:**
- Modify: `crates/lr-cli/src/commands/jobs.rs` (add the function next to `print_warnings`, call it in `submit`)
- Modify: `crates/lr-cli/src/commands/mod.rs:74-76` (help text of `--detach`)
- Create: `crates/lr-cli/tests/submit_detach_warning.rs`

**Interfaces:**
- Produces: `pub const DETACH_WARNING: &str` (the text of decision 5) and
  `pub fn warn_if_orphaned(detach: bool, wait: bool, terminal: bool, err: &mut impl std::io::Write)` in `lr_cli::commands::jobs`: writes `DETACH_WARNING` plus a newline to `err` only when all of `detach`, `wait`, `terminal` are false.

- [ ] **Step 1: Write the failing tests** in `crates/lr-cli/tests/submit_detach_warning.rs`, using `lr_cli::commands::jobs::{warn_if_orphaned, DETACH_WARNING}` and a `Vec<u8>` as `err`:
  - `warns_once_without_detach_wait_or_terminal`: `warn_if_orphaned(false, false, false, &mut buf)`; assert `String::from_utf8(buf) == format!("{DETACH_WARNING}\n")`.
  - `no_warning_with_detach`: `(true, false, false)` leaves `buf` empty.
  - `no_warning_with_wait`: `(false, true, false)` leaves `buf` empty (covers `run`, which calls `submit` with `wait = true`).
  - `no_warning_with_a_terminal`: `(false, false, true)` leaves `buf` empty.
  - `warning_text_is_one_line_naming_the_cure`: `DETACH_WARNING` has no `\n`, contains `--detach`, `localrunner wait` and `localrunner run`.
  - `detach_flag_help_states_the_default`: `use clap::CommandFactory;` then `Cli::command()` -> `find_subcommand("submit")` -> `get_arguments()` -> the argument with `get_id() == "detach"`; `get_help().unwrap().to_string()` contains `cancelled when the sending process exits`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lr-cli --test submit_detach_warning`
Expected: compile FAIL (`warn_if_orphaned` / `DETACH_WARNING` not found).

- [ ] **Step 3: Implement**
  - In `jobs.rs`, add `DETACH_WARNING` and `warn_if_orphaned` with `///` doc comments (what and when, no more).
  - In `jobs::submit`, keep the `submit_on` call, bind its result, and when it is `Ok(0)`, call `warn_if_orphaned(args.detach, wait, std::io::stdin().is_terminal(), &mut std::io::stderr())` (import `IsTerminal` next to `Read, Write`; `args.detach` is `Copy`, read it into a local before building the request so the order of field moves does not matter; `warn_if_orphaned` itself does nothing when `wait` is true), then return the result. Rejections (`Ok(1)`) and errors print no warning.
  - In `mod.rs` change the doc comment of `OrderArgs::detach` to: `Without it the order is cancelled when the sending process exits; with it the order outlives the sending process.`

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p lr-cli --test submit_detach_warning --test submit_done --test cli_surface`
Expected: all PASS (`submit_done` proves the `submit_on` stdout behaviour is untouched).

- [ ] **Step 5: Commit**

```bash
git add crates/lr-cli/src/commands/jobs.rs crates/lr-cli/src/commands/mod.rs crates/lr-cli/tests/submit_detach_warning.rs
git commit -m "feat(cli): warn on stderr when submit without --detach has no terminal"
```

### Task 2: The `run` skill states the rule

**Files:**
- Modify: `plugin/skills/run/SKILL.md` (steps 3, 4 and the last sentence of step 6)
- Test: `crates/lr-cli/tests/skills_lint.rs` (new test next to the existing ones; it already has `skill(name)` and `command_spans`)

**Interfaces:**
- Consumes: `skill("run") -> String` from `skills_lint.rs`.

- [ ] **Step 1: Write the failing test** `run_skill_states_the_detach_rule` in `skills_lint.rs`: `let text = skill("run");` assert it contains `Without `--detach` the order is cancelled when the sending process exits` and `always passes `--detach` to `submit``; assert it does NOT contain `only when the run must outlive the sending process`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lr-cli --test skills_lint run_skill_states_the_detach_rule`
Expected: FAIL.

- [ ] **Step 3: Edit the skill.**
  - Step 3: after the `submit ... ` command, add the sentence: `Without `--detach` the order is cancelled when the sending process exits, and a shell call of an agent session ends right after `submit` returns the job id: an agent session always passes `--detach` to `submit` and then waits, or uses `run`.` and change the example to `localrunner submit [profile] --detach --as <label> ...`.
  - Step 4: `submit --detach` prints the job id, then `localrunner wait <job-id>` as a background task (rest unchanged).
  - Step 6: delete `Add `--detach` to `submit` only when the run must outlive the sending process.`
  - Every `` `localrunner ...` `` span must still parse (the lint checks flags against clap).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p lr-cli --test skills_lint`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add plugin/skills/run/SKILL.md crates/lr-cli/tests/skills_lint.rs
git commit -m "docs(skill): run skill says submit without --detach is cancelled on sender exit"
```

### Task 3: README pair and architecture pair

**Files:**
- Modify: `README.md:541`, `README.ru.md:544` (the `--detach` item of the flags line)
- Modify: `docs/architecture.en.html:953` and `:772`; `docs/architecture.html:953` and `:772` (the `submit` command row, and the "Dead sender" bullet in the same places)

- [ ] **Step 1: README pair.** Replace the `--detach` item with: `--detach` (without it the order is cancelled when the sending process exits; an agent session always passes it and then `wait`s, or uses `run`). `README.ru.md`: `--detach` (без него заказ снимается, когда процесс-отправитель завершается; сессия агента всегда передаёт его и затем ждёт через `wait` или использует `run`).

- [ ] **Step 2: Architecture pair, row 953.** In the `submit` row, replace the clause "with `--detach` the order (waiting or running) outlives the sender" with: "without `--detach` the order is cancelled when the sender exits, with `--detach` the order (waiting or running) outlives the sender; without `--detach`, `--wait` and a terminal, one warning line goes to stderr". Russian file: replace "с `--detach` заказ (ждущий и выполняющийся) переживает отправителя" with "без `--detach` заказ снимается при завершении отправителя, с `--detach` заказ (ждущий и выполняющийся) переживает отправителя; без `--detach`, `--wait` и терминала в stderr выводится одна строка предупреждения". Keep `<code>` around flag and command names as in the row, and keep each edit on its own existing line (the CI docs job compares line and tag counts of the pair).

- [ ] **Step 3: Architecture pair, line 772.** Append to the "Dead sender" bullet: "An agent session (short-lived shell calls) always passes `--detach` and then `wait`s, or uses `run`." Russian: "Сессия агента (короткие вызовы оболочки) всегда передаёт `--detach` и затем ждёт через `wait` или использует `run`." Wrap the flag and command names in `<code>`, add no new `<li>`, `<tr>` or `id` attributes."

- [ ] **Step 4: Verify the pairs agree**

Run: `git diff --stat` and `grep -c "detach" README.md README.ru.md docs/architecture.html docs/architecture.en.html` and confirm each pair changed in the same lines (README 1 line each, architecture 2 lines each).
Expected: both members of every pair appear in the diff.

- [ ] **Step 5: Commit**

```bash
git add README.md README.ru.md docs/architecture.html docs/architecture.en.html
git commit -m "docs: submit without --detach is cancelled when the sender exits"
```

### Task 4: Full verification

- [ ] **Step 1:** Run `cargo fmt --check`, `cargo clippy -p lr-cli --all-targets -- -D warnings`, `cargo test -p lr-cli`. Expected: all clean/PASS.
- [ ] **Step 2:** `wc -l crates/lr-cli/src/commands/jobs.rs` is under 500.
- [ ] **Step 3:** Push the branch and open the PR titled `docs: say that submit without --detach is cancelled when the sender exits` with `Closes #71`, English body, no attribution lines.

## Self-review

- Spec coverage: SKILL.md (Task 2), docs (Task 3), warning (Task 1), docs template (skipped, decision 3), help text (Task 1).
- Wiring of `jobs::submit` to the real stdin/stderr is not unit-tested (it needs a hub); the pure function and the call condition `Ok(0)` carry the coverage.
- Names match across tasks: `warn_if_orphaned`, `DETACH_WARNING`.
- Known limitation, to state in the PR: in a terminal there is no warning, although `submit` without `--detach` is cancelled there too once the command returns (the owner's rule "without a terminal" is followed as decided).
