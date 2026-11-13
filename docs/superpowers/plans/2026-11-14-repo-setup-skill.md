# Skill localrunner:repo-setup and `profile check` extension Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a consumer repository an interactive setup skill `localrunner:repo-setup`, and make `localrunner profile check` a real profile validator (per-profile summary, zero-profile failure, ignored-section warnings, `--file`, `--json`) that the skill relies on.

**Architecture:** Two deliverables in one PR. (A) `lr-core` gains `ignored_sections` (top-level keys the parser silently drops); `lr-cli` gains a `profile_summary` module (summary model, text and JSON rendering) and `profile_cmd::check` is rewired to print the summary first and the unchanged secrets report after it. (B) A markdown-only skill folder `plugin/skills/repo-setup/` (`SKILL.md` + `reference.md`) checked by `skills_lint.rs`; the skill delegates correctness to `profile check`.

**Tech Stack:** Rust (workspace crates `lr-core`, `lr-cli`), clap derive, serde/serde_json, toml; Markdown skills; HTML docs.

**Spec:** issue #33 and the decisions below (owner decisions of 2026-11-13).

## Decisions (approved, do not reopen)

- Removed from issue #33 by the owner, and absent from the skill: plugin installation and plugin scope talk; GitHub Actions workflows and any mention of GitHub or other CI systems; the `Job launches:` rule and any edit of agent instruction files (`AGENTS.md`, `CLAUDE.md`); `docs/runners.md`; the "two different ci labels" pitfall.
- Skill behavior:
  - Quietly detects state: `.localrunner/profiles.toml`, `.gitignore`.
  - New repository: first asks whether to analyze the repository or whether the user will say what is heavy; reads project files only after consent.
  - Configured repository: first asks what to change or what is missing; audit is the fallback. Idempotent; never overwrites existing settings.
  - Uncovers needs by scenarios (what is heavy, what disturbs parallel sessions, services, secrets) and proposes a solution.
  - Writes `.localrunner/profiles.toml` and `temp/` in `.gitignore` ONE FILE AT A TIME, showing the exact diff and waiting for confirmation; touches nothing else in a file.
  - Numbers (`mem_mb`, `cpu`, `timeout_s`): reads `localrunner runners`, shows machine limits, asks the user; if unknown proposes cautious values (half of the machine, 15 min timeout = `timeout_s = 900`) labelled a guess; refuses a profile that does not fit the runner.
  - Asks explicitly "by commit" (profile with `platforms`) vs "by work tree" and checks the runner's platforms.
  - Machine side: shows the exact diff for `runner.toml`/`policy.toml`, applies only after explicit consent, verifies with `localrunner doctor` and `localrunner policy check`, asks whether the user is ready for UAC and waits, never sets `allow_elevated` or `allow_same_user_as_broker`, never prints secrets.
  - Ending: `localrunner profile check`, `localrunner doctor`, then OFFERS a trial `localrunner run <profile> --as <label>` (never automatic).
- `profile check` extension: summary per profile (name, steps or command, timeout, resources, platform mode, arguments); fails when the file has zero profiles; warns about ignored top-level sections (such as `[profiles.x]`); optional file path via `--file <path>`; `--json`. Existing secrets output and exit codes stay.
- Platform mode: "by commit" when the profile has `platforms` or any `platform.*` override, else "by work tree".
- Non-goals: no validation of secret values or commands; no change to the daemon, queue or scheduler; no CI, language or toolchain knowledge anywhere.

## Global Constraints

- GitHub texts, commit messages, AGENTS.md and all AI instructions are in English; no `Co-Authored-By` or "Generated with" in commits.
- No code comments except doc comments on public items.
- Files stay under 500 lines (excluding `docs/architecture*.html`).
- Docs pairs change together in the same commit: `docs/architecture.html` + `docs/architecture.en.html`; `README.md` + `README.ru.md`.
- Generic core: nothing in `lr-core` names a consumer, language, toolchain or test framework.
- Skill frontmatter `name` equals the folder; `description` has Russian and English triggers; skill texts in English; every `localrunner ...` span in backticks must exist in the clap definition (`crates/lr-cli/tests/skills_lint.rs`).
- Do not run `cargo build` or `cargo test` for the whole workspace; run only the named package and test target. Plugins at project scope only.

## Review Focus

- Profile file with only a typo section (`[profiles.x]`): exit 1, message names the section and says the table is `[profile.<name>]`.
- Empty file or file with comments only: exit 1 (zero profiles), not a crash.
- TOML syntax error or invalid field: exit 1 as before, the parser message is printed unchanged; with `--json` the error still goes to stderr and stdout stays empty.
- `--json` stdout is a single JSON document and nothing else (no secret lines mixed in); text mode prints nothing to stdout in JSON mode.
- `[needs.*]`-inherited `platforms` count for platform mode (the summary reads the resolved `Profile`).
- Profile filter with an unknown name keeps the existing error `no profile <name> in <file>`; the ignored-section warning is not lost when a filter is given.
- A skill text that names a flag or command not in clap fails `skills_lint`, so a typo in the skill is caught.

---

## File Structure

- Modify `crates/lr-core/src/profile_parse.rs`, `crates/lr-core/src/lib.rs`: `ignored_sections`.
- Create `crates/lr-core/tests/profile_ignored.rs`.
- Create `crates/lr-cli/src/commands/profile_summary.rs`: summary model and rendering. Register in `crates/lr-cli/src/commands/mod.rs`.
- Modify `crates/lr-cli/src/commands/profile_cmd.rs`, `args.rs` (`ProfileAction::Check`), `mod.rs` (dispatch).
- Create `crates/lr-cli/tests/profile_summary.rs` (unit-level) and `crates/lr-cli/tests/profile_check_cli.rs` (binary-level). Existing `tests/profile_check.rs` stays untouched and must still pass.
- Create `plugin/skills/repo-setup/SKILL.md`, `plugin/skills/repo-setup/reference.md`.
- Modify `crates/lr-cli/tests/skills_lint.rs`, `docs/architecture.html`, `docs/architecture.en.html`, `README.md`, `README.ru.md`.

### Task 1: `ignored_sections` in lr-core

**Files:**
- Modify: `crates/lr-core/src/profile_parse.rs`, `crates/lr-core/src/lib.rs` (add to `pub use profile_parse::{...}`)
- Test: `crates/lr-core/tests/profile_ignored.rs`

**Interfaces:**
- Produces: `pub fn ignored_sections(text: &str) -> Result<Vec<String>, ProfileError>`: the top-level keys of the TOML document other than `profile` and `needs`, sorted; `ProfileError::Syntax` on invalid TOML. It does not change `parse_profiles` (which keeps ignoring unknown sections).

- [ ] **Step 1: Write the failing tests** in `crates/lr-core/tests/profile_ignored.rs`:

```rust
use lr_core::{ignored_sections, parse_profiles};

const TYPO: &str = "[profiles.x]\ncommand = \"true\"\ntimeout_s = 5\n";

#[test]
fn a_typo_section_is_reported_and_parsing_still_ignores_it() {
    assert_eq!(ignored_sections(TYPO).unwrap(), vec!["profiles".to_string()]);
    assert!(parse_profiles(TYPO).unwrap().is_empty());
}

#[test]
fn known_sections_and_top_level_scalars() {
    let text = "[profile.a]\ncommand = \"true\"\ntimeout_s = 5\n[needs.n]\ntimeout_s = 5\n";
    assert!(ignored_sections(text).unwrap().is_empty());
    let text = format!("version = 1\n[zzz]\n[aaa]\n{text}");
    assert_eq!(ignored_sections(&text).unwrap(), ["aaa", "version", "zzz"]);
}

#[test]
fn invalid_toml_is_a_syntax_error() {
    assert!(ignored_sections("[profile").is_err());
}
```

- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p lr-core --test profile_ignored` Expected: FAIL (compile error, `ignored_sections` not found).
- [ ] **Step 3: Implement** `ignored_sections` in `profile_parse.rs` (parse into `toml::Table`, filter keys, map error to `ProfileError::Syntax(e.to_string())`), with a doc comment; re-export from `lib.rs`.
- [ ] **Step 4: Run to verify pass.** Run: `cargo test -p lr-core --test profile_ignored` Expected: 3 passed. Also `cargo test -p lr-core` (single package) stays green.
- [ ] **Step 5: Commit**

```bash
git add crates/lr-core
git commit -m "feat(core): report top-level profile file sections the parser ignores"
```

### Task 2: Summary model and rendering

**Files:**
- Create: `crates/lr-cli/src/commands/profile_summary.rs` (`pub mod profile_summary;` in `commands/mod.rs`)
- Test: `crates/lr-cli/tests/profile_summary.rs`

**Interfaces:**
- Consumes: `lr_core::Profile`, `StepSpec`, `ArgSpec` (fields as in `crates/lr-core/src/profile.rs`).
- Produces (all `pub`, with doc comments, `#[derive(Serialize)]` on the data types):
  - `struct StepSummary { name: String, command: String, timeout_s: u64 }`
  - `struct ProfileSummary { name: String, timeout_s: u64, command: Option<String>, steps: Vec<StepSummary>, mem_mb: Option<u64>, cpu: Option<u32>, disk_mb: Option<u64>, platform_mode: &'static str, platforms: Vec<String>, args: Vec<String> }`. `command` is `Some` when `from_command`, and then `steps` is empty; `platform_mode` is `"by commit"` or `"by work tree"`; `platforms` is the union of `platforms` and the override keys, sorted; `args` are the argument names.
  - `fn summarize(p: &Profile) -> ProfileSummary`
  - `fn summary_lines(list: &[ProfileSummary]) -> Vec<String>`: per profile, line `profile <name>: <platform_mode>[ (<platforms joined by ", ">)], timeout <n> s`, then indented lines `  command: <c>` or `  step <name>: <command> (timeout <n> s)`, `  resources: mem <n> MB, cpu <n>, disk <n> MB` (omit unset parts; `resources: none declared` when all unset), `  arguments: a, b` (only when present).

- [ ] **Step 1: Write failing tests** (parse the TOML with `lr_core::parse_profiles`):
  - `a_command_profile_is_summarized`: profile `command = "cargo test"`, `timeout_s = 900`, `mem_mb = 4096`, `cpu = 2`; assert `command == Some("cargo test")`, `steps` empty, `platform_mode == "by work tree"`, and the lines contain `"profile unit: by work tree, timeout 900 s"`, `"  command: cargo test"`, `"  resources: mem 4096 MB, cpu 2"`.
  - `a_steps_profile_lists_every_step`: two steps; assert the lines contain `"  step lint: cargo clippy (timeout 60 s)"` and the second step.
  - `platforms_and_platform_tables_mean_by_commit`: three profiles: `platforms = ["linux"]`; only `[profile.p.platform.ios]`; platforms inherited from `needs = "n"` with `[needs.n] platforms = ["linux"]`. All three: `platform_mode == "by commit"`; the first line contains `by commit (linux)`.
  - `arguments_and_missing_resources`: `[profile.t.args.target]` present gives `"  arguments: target"` and `"  resources: none declared"`.
  - `the_summary_serializes_with_snake_case_keys`: `serde_json::to_value(&summarize(..))` has `platform_mode` and `timeout_s`.
- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p lr-cli --test profile_summary` Expected: FAIL (compile error).
- [ ] **Step 3: Implement** the module per the interfaces above.
- [ ] **Step 4: Run to verify pass.** Run: `cargo test -p lr-cli --test profile_summary` Expected: 5 passed.
- [ ] **Step 5: Commit**

```bash
git add crates/lr-cli
git commit -m "feat(cli): per-profile summary model for profile check"
```

### Task 3: Rewire `profile check` (file path, JSON, zero profiles, warnings)

**Files:**
- Modify: `crates/lr-cli/src/commands/args.rs` (`ProfileAction::Check`), `crates/lr-cli/src/commands/mod.rs` (the `Command::Profile { action }` arm, lines ~500-504, and `pub mod profile_summary;`), `crates/lr-cli/src/commands/profile_cmd.rs`
- Test: `crates/lr-cli/tests/profile_check_cli.rs`

**Interfaces:**
- Consumes: `ignored_sections` (Task 1), `summarize`, `summary_lines`, `ProfileSummary` (Task 2), existing `report`, `Machine`.
- Produces:
  - `ProfileAction::Check { profile: Option<String>, label: Option<String>, file: Option<PathBuf>, json: bool }`; `--file <path>` (default `.localrunner/profiles.toml`), `--json`. Update the doc comment of the variant: it now checks the file and the secrets.
  - `pub fn check(layout: &Layout, profile: Option<&str>, label: Option<&str>, file: Option<&Path>, json: bool) -> Result<i32, String>` and `pub fn run(layout: &Layout, action: ProfileAction) -> Result<i32, String>`, which destructures `ProfileAction::Check` and calls `check`. `mod.rs` is already 514 lines (over the 500 limit), so it must not grow: the arm becomes `Command::Profile { action } => profile_cmd::run(&layout, action),` (3 lines fewer, offsetting the one added `pub mod` line); never add the multi-line destructuring there.
  - `pub fn verdict(profiles: &[Profile], ignored: &[String], secrets_ok: bool) -> bool`: false when `profiles` is empty or `secrets_ok` is false; warnings never fail.
  - Text output order: warning lines first (`warning: ignored top-level section [<name>]: only [profile.<name>] and [needs.<name>] are read`), then summary lines, then the unchanged secrets report lines. Zero profiles prints to stdout `no profiles in <path>` (plus the warning lines) and exits 1. The filter-by-name error and parse errors stay `Err` (exit 1 via `eprintln!("error: ...")`).
  - JSON output (one document on stdout, nothing else): `{"file": "<path>", "ok": bool, "profiles": [ProfileSummary...], "ignored_sections": [..], "secrets": {"ok": bool, "lines": [..]}}`.

- [ ] **Step 1: Write failing tests** in `crates/lr-cli/tests/profile_check_cli.rs`, running the binary like `commands.rs` does (`env!("CARGO_BIN_EXE_localrunner")`, env `LOCALRUNNER_STATE_DIR` set to a `common::TempDir`), writing the profiles file into a `TempDir` and passing `--file`:
  - `a_good_file_prints_the_summary_then_the_secrets_report_and_exits_0`: stdout contains `profile unit: by work tree, timeout 900 s` and `no profile asks for a secret`; the summary line index is before the secrets line.
  - `zero_profiles_fails`: empty file gives exit 1 and stdout contains `no profiles in`.
  - `a_typo_section_warns_and_fails_on_zero_profiles`: file `[profiles.x]...` gives exit 1, stdout contains `ignored top-level section [profiles]`.
  - `a_typo_section_next_to_a_good_profile_warns_and_exits_0`.
  - `a_syntax_error_fails_with_the_parser_message_on_stderr`: exit 1, stderr contains `profiles.toml:`.
  - `json_is_one_document`: `--json` stdout parses with `serde_json::from_slice`; `profiles[0].platform_mode == "by commit"` for a profile with `platforms = ["linux"]`; `ok == true`; `ignored_sections == []`; stdout has no non-JSON text.
  - `json_for_zero_profiles_has_ok_false_and_exit_1`.
  - `an_unknown_profile_name_keeps_the_old_error`: `unit2` gives exit 1 and stderr contains `no profile unit2 in`.
  - A pure test for `verdict` (in `tests/profile_summary.rs` or here): empty profiles false; non-empty with failing secrets false; non-empty with ignored sections true.
- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p lr-cli --test profile_check_cli` Expected: FAIL (unknown `--file`/`--json`).
- [ ] **Step 3: Implement.** In `check`: read the file (default path), `parse_profiles`, `ignored_sections`, apply the name filter (error as today), build `Machine` exactly as today, call existing `report`, then render text or JSON. Keep `profile_cmd.rs` under 500 lines; the rendering helpers for JSON live in `profile_summary.rs` if `profile_cmd.rs` grows beyond ~300 lines.
- [ ] **Step 4: Run to verify pass.** `wc -l crates/lr-cli/src/commands/mod.rs` must not exceed its starting 514. Run: `cargo test -p lr-cli --test profile_check_cli --test profile_check --test profile_summary --test skills_lint` Expected: all pass (the old `profile_check` tests prove the secrets report is unchanged).
- [ ] **Step 5: Commit**

```bash
git add crates/lr-cli
git commit -m "feat(cli): profile check prints a summary, fails on zero profiles, takes --file and --json"
```

### Task 4: Document `profile check` in both docs and both READMEs

**Files:**
- Modify: `docs/architecture.en.html` and `docs/architecture.html` (section 11.1, table "Configuration and maintenance": add a row right after the `policy check [file]` row; the existing documents have no `profile check` row yet), `README.md` (row at line ~536), `README.ru.md` (row at line ~539).

**Interfaces:** none (docs).

- [ ] **Step 1: Add the rows.** Command cell: `profile check [profile] [--label L] [--file path] [--json]`. Text (English): validates the profiles file (default `.localrunner/profiles.toml`): prints a per-profile summary (steps or command, timeout, resources, platform mode "by commit" or "by work tree", arguments), warns about ignored top-level sections such as `[profiles.x]`, then checks the secrets as before; exit 1 for a parse error, zero profiles or a secret that would stop a run. Russian files get the same row in Russian. In `README*`, extend the existing row instead of adding a second one.
- [ ] **Step 2: Verify the pairs match.** Run: `git diff --stat docs README.md README.ru.md` Expected: four files changed, each with the same number of added rows; compare the command cell text by eye.
- [ ] **Step 3: Commit**

```bash
git add docs README.md README.ru.md
git commit -m "docs: profile check summary, --file and --json"
```

### Task 5: The skill, its reference, lint and tables

**Files:**
- Create: `plugin/skills/repo-setup/SKILL.md`, `plugin/skills/repo-setup/reference.md`
- Modify: `crates/lr-cli/tests/skills_lint.rs`, `docs/architecture.html`, `docs/architecture.en.html` (section 11.2 table), `README.md` (skills table near line 75), `README.ru.md` (skills table near line 75)
- Test: `crates/lr-cli/tests/skills_lint.rs`

**Interfaces:**
- Consumes: the flags `--file`, `--json` of Task 3 (the skill names them inside full backticked `localrunner profile check ...` spans, so the lint proves they exist).
- Produces: skill `repo-setup`.

- [ ] **Step 1: Write the failing test first.** In `skills_lint.rs` change `SKILLS` to `[&str; 8]` and insert `"repo-setup"` between `"install-runner"` and `"run"` (the test compares with the sorted directory listing). In the same file add a test `repo_setup_keeps_its_scope`:

```rust
#[test]
fn repo_setup_keeps_its_scope() {
    let text = skill("repo-setup") + &std::fs::read_to_string(plugin().join("skills/repo-setup/reference.md")).unwrap();
    for word in ["GitHub", "workflow", "CI ", "AGENTS.md", "CLAUDE.md", "docs/runners.md", "plugin install", "allow_elevated = true"] {
        assert!(!text.contains(word), "{word} is out of scope for repo-setup");
    }
    for cmd in ["`localrunner profile check`", "`localrunner profile check --json`", "`localrunner profile check --file <path>`", "`localrunner doctor`", "`localrunner policy check`", "`localrunner runners`"] {
        assert!(text.contains(cmd), "{cmd}");
    }
}
```

  Note: the skill may mention `allow_elevated` and `allow_same_user_as_broker` only as "never set"; the test therefore forbids only the literal `allow_elevated = true`.
- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p lr-cli --test skills_lint` Expected: FAIL (missing `repo-setup/SKILL.md`).
- [ ] **Step 3: Write `SKILL.md`** (English; frontmatter `name: repo-setup`; `description` folded scalar like the other skills with triggers: "set up localrunner in this repo", "configure profiles", "настрой localrunner в репозитории", "добавь профили раннера", "что тяжёлое запускать через раннер"). Body, numbered, imperative, in the style of `plugin/skills/run/SKILL.md`; it must contain exactly these rules:
  1. Detect quietly: does `.localrunner/profiles.toml` exist, is `temp/` in `.gitignore`. Read nothing else yet.
  2. New repository: first ask "analyze the repository, or you tell me what is heavy?"; read project files only after the user chooses analysis. Configured repository: first ask what to change or what is missing; offer an audit (`localrunner profile check`, then a read of the file) only when there is no request. Reading project files to find what is missing needs the same consent as in a new repository. Never overwrite or reformat existing entries; add or change only what was asked.
  3. Uncover needs by scenarios: what is heavy, what disturbs parallel sessions, which services, which secrets; propose profiles; keep the fields to the reference (`reference.md`).
  4. Numbers: run `localrunner runners`, show memory, CPU and platforms; ask for `mem_mb`, `cpu`, `timeout_s`; when the user does not know, propose half of the machine and `timeout_s = 900` and label it a guess; refuse a profile that does not fit the runner and say which limit is exceeded.
  5. Mode: ask explicitly "by commit (profile has `platforms`, HEAD must be pushed, the runner must declare the platform) or by work tree"; check the runner's platforms from `localrunner runners`.
  6. Write one file at a time: show the exact diff of `.localrunner/profiles.toml`, wait for confirmation, write; then the same for `.gitignore` (`temp/` only, one line, no duplicate); touch nothing else in the file.
  7. Machine side only when a profile needs it (service, secret source, tag, program): show the exact diff for `runner.toml` or `policy.toml`, apply only after explicit consent, verify with `localrunner doctor` and `localrunner policy check`; before anything that needs elevation ask whether the user is ready for the UAC prompt and wait; never set `allow_elevated` or `allow_same_user_as_broker`; never print, store or ask to paste secret values (secrets are `env:` names only).
  8. Ending: run `localrunner profile check` (`localrunner profile check --json` when parsing the result; `localrunner profile check --file <path>` for a file elsewhere; write each flag inside a full backticked `localrunner ...` span, otherwise `skills_lint` does not check it), then `localrunner doctor`; fix what they report; then OFFER a trial `localrunner run <profile> --as <label>` with a clear label like `claude:repo-setup`; never start it without a yes. Correctness of a profile is decided by `profile check`, not by the reference.
  9. Scope sentence: the skill configures only what localrunner owns (`.localrunner/profiles.toml`, `temp/` in `.gitignore`, and on consent `runner.toml` and `policy.toml`) and edits nothing else. Do not name other systems or files it does not touch.
- [ ] **Step 4: Write `reference.md`** (short, under 120 lines): fields of `[profile.<name>]` and `[needs.<name>]` (`command` xor `steps`; `timeout_s` required; `mem_mb`, `cpu`, `disk_mb`, `platforms`, `programs`, `tags`, `exclusive`, `cache`, `services`, `secrets`, `zone`, `needs`, `platform.<P>`, `args.<name>`; step keys `name`, `command`, `timeout_s`, `continue_on_failure`; absent: `env`, working directory, `matrix`, retry), and these pitfalls: (1) top-level only `[profile.*]` and `[needs.*]`, others are ignored and `profile check` warns; (2) profile with `platforms` or `platform.*` is a "by commit" order: `HEAD` must be pushed and the runner must declare the platform in its `runner.toml`, where the template has `platforms` commented out; without platforms the profile runs by work tree; (3) `keychain:` secret sources are parsed but not read, use `env:`; (4) reserved secret names `PATH`, `HOME`, `TEMP`, `TMP`, `LOCALRUNNER_*` fail the job; (5) the `check` of a profile's `programs` runs only on the runner, per its own `[programs]`; (6) masking is job-wide and exact, do not use short values such as `true` or `1` as secrets. Machine files: names of the `runner.toml` and `policy.toml` keys the skill may propose (`platforms`, `tags`, `[programs]`, `[secret.*]`, `[service.*]`, `[[secrets.allow]]`, `[[services.allow]]`), taken from sections 3.5, 3.10, 3.11, 3.13 (machine files) and 5 (profile fields) of `docs/architecture.en.html`; do not invent keys. Check each key against the document before writing it.
- [ ] **Step 5: Add the skill to the tables.** Section 11.2 of `docs/architecture.en.html` and `docs/architecture.html`: a row `repo-setup` with Input "what to change (optional), the repository state" and Result "`.localrunner/profiles.toml` and `temp/` in `.gitignore` written file by file after confirmation of the diff; machine files changed only on explicit consent; `profile check` and `doctor` pass; a trial run offered", placed after `run`. `README.md` and `README.ru.md` skills tables: row `"set up localrunner in this repository"` / `repo-setup` / "interviews, writes the profiles file and `temp/` in `.gitignore` after you confirm each diff, checks it with `profile check`" and its Russian twin.
- [ ] **Step 6: Run to verify pass.** Run: `cargo test -p lr-cli --test skills_lint` Expected: all pass (every `localrunner ...` span of the skill resolves in clap, including `--file` and `--json` on `profile check`).
- [ ] **Step 7: Commit**

```bash
git add plugin/skills/repo-setup crates/lr-cli/tests/skills_lint.rs docs README.md README.ru.md
git commit -m "feat(plugin): skill localrunner:repo-setup"
```

### Task 6: Final verification (no commit)

- [ ] **Step 1:** `cargo test -p lr-core --test profile_ignored`, `cargo test -p lr-cli --test profile_summary --test profile_check_cli --test profile_check --test skills_lint`. Expected: all pass.
- [ ] **Step 2:** `cargo fmt --check -p lr-core -p lr-cli` and `cargo clippy -p lr-core -p lr-cli --all-targets -- -D warnings`. Expected: clean.
- [ ] **Step 3:** `wc -l` on every touched `.rs` and `.md` file under `crates/` and `plugin/`: each under 500.
- [ ] **Step 4:** `git log --format=%B origin/main..` shows no attribution lines; the PR body (when the owner asks for it) is English and ends with `Closes #33`.
