# Secret Masking Scope Documentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Document that secret masking in job output is job-wide and exact, and that a short or common secret value damages ordinary output, so such values must not be used as secrets.

**Architecture:** No behavior change. The architecture document pair (`docs/architecture.html` Russian, `docs/architecture.en.html` English) gets the rule in section 3.13 "Job secrets" next to the existing masking guarantee; `README.md` and `README.ru.md` get one matching sentence where masking is already mentioned. Each pair changes in one commit.

**Tech Stack:** HTML and Markdown documentation only. No Rust change, no cargo run.

**Spec:** issue #74 and the decisions below.

Decisions (approved by the owner, not to be reopened):
- Masking stays as it is: the exact value is replaced by `***`; the whole job output is masked (every step's output, stdout and stderr, is masked with the secrets of all steps of the job, not only those of the step that requested them); no minimum length; no per-step scope; no log line telling which secret matched; no warning in `profile check`.
- Only documentation changes: masking is job-wide and exact; a short or common value (`true`, `1`, `K`) damages ordinary output; do not use such values as secrets.
- The cause of the original report is not investigated further by decision.

Verified facts in code (the documentation must match them):
- `crates/lr-exec/src/job.rs`: `run_job` builds `Job.secrets` from `spec.secret_values()`; `spawn_step(&launch, &self.secrets)` passes that job-wide list to the stdout and stderr maskers, and `Job::line` and the "step not started" message use the same list.
- `crates/lr-abstractions/src/process.rs`, `JobSpec::secret_values`: "Every secret value of every step, for masking."
- `crates/lr-exec/src/mask.rs`, `Masker`: exact byte match, empty values ignored, longer values win over their prefixes, no minimum length.
- `mask_secrets` (`process.rs:431`) doc comment: "Replaces every occurrence of each non-empty secret value in `data` with [`MASK`]". This is accurate; it is not changed.

## Global Constraints

- Respond in Russian to the owner; the plan, commit messages, PR text and the English documents are in English; Russian only in `docs/architecture.html` and `README.ru.md`.
- A change to one file of a pair is made in the other in the same commit (`docs/architecture.html` + `docs/architecture.en.html`; `README.md` + `README.ru.md`).
- No agent attribution (`Co-Authored-By`, "Generated with") in commits and the pull request.
- No code comments added; no new Markdown design files (this plan is the only new file); files under 500 lines except the two architecture HTML files.
- The pull request body contains `Closes #74`; no separate task for commenting on or closing the issue.
- Mask token is `***`.

## Review Focus

- Wording says "exact value", not "pattern": a reader must not infer partial or case-insensitive matching.
- Wording says job-wide, so a secret requested by step A also masks step B's output although B did not request it (contradiction risk with the nearby sentence "a secret is visible only ... on the steps that requested it", which concerns the environment, not masking).
- Examples `true`, `1`, `K` are given as values that damage output (for example `fun <K, V> ...` becomes `fun <***, V> ...`).
- The Russian and English texts say the same thing; no claim of a minimum length, a warning or a "which secret matched" line.
- No other place in the docs claims per-step masking or a length rule (checked by the grep in Task 1).

---

### Task 1: Architecture document pair, section 3.13

**Files:**
- Modify: `docs/architecture.en.html` (section 3.13 "Job secrets", the `<li><b>Guarantees:</b>` item at line ~588)
- Modify: `docs/architecture.html` (the same section, `<li><b>Гарантии:</b>` item at line ~588)

- [ ] **Step 1: Confirm that no document claims otherwise**

Run: `grep -n -i -E "min(imum)? length|per-step mask|по шагам.*маскир|минимальн.*длин" docs/architecture.html docs/architecture.en.html README.md README.ru.md`
Expected: no output about masking (any hit must be reconciled with the decisions above before continuing).

- [ ] **Step 2: Add a new `<li>` directly after the "Guarantees" item in `docs/architecture.en.html`**

Exact text:

```html
<li><b>Scope of masking.</b> Masking is job-wide and exact. The runner replaces the exact value of every secret of the job with <code>***</code> in the output of every step (stdout and stderr), including a step that did not request that secret; the environment is per step, masking is not. There is no minimum length, no pattern and no per-step scope, and the log does not say which secret matched. A short or common value (<code>true</code>, <code>1</code>, <code>K</code>) therefore also masks the same characters inside ordinary output, for example <code>fun &lt;K, V&gt;</code> becomes <code>fun &lt;***, V&gt;</code>, and damages names and messages in the log, including the name of a failing test. Do not use such values as secrets: use a long random value (a token, a key).</li>
```

- [ ] **Step 3: Add the Russian equivalent to `docs/architecture.html`, same position**

Exact text:

```html
<li><b>Область маскировки.</b> Маскировка действует на всю джобу и только на точное значение. Раннер заменяет точное значение каждого секрета джобы на <code>***</code> в выводе каждого шага (stdout и stderr), в том числе шага, который этот секрет не запрашивал: окружение задаётся по шагам, маскировка — нет. Минимальной длины, шаблонов и области по шагам нет, а лог не сообщает, какой секрет совпал. Поэтому короткое или распространённое значение (<code>true</code>, <code>1</code>, <code>K</code>) маскирует те же символы и внутри обычного вывода, например <code>fun &lt;K, V&gt;</code> превращается в <code>fun &lt;***, V&gt;</code>, и портит имена и сообщения в логе, в том числе имя упавшего теста. Такие значения секретами использовать нельзя: берите длинное случайное значение (токен, ключ).</li>
```

- [ ] **Step 4: Sync the log command row (line ~961) in both files**

English `log` row: replace `secrets are masked` with `secrets are masked (exact values, job-wide, see 3.13)`.
Russian `log` row: replace `секреты маскируются` with `секреты маскируются (точные значения, на всю джобу, см. 3.13)`.

- [ ] **Step 5: Verify**

Run: `grep -c "Scope of masking" docs/architecture.en.html; grep -c "Область маскировки" docs/architecture.html; grep -n "job-wide, see 3.13" docs/architecture.en.html; grep -n "на всю джобу, см. 3.13" docs/architecture.html`
Expected: `1`, `1`, one line each.
Then open both files' section 3.13 (read the surrounding lines) to confirm the list is still well-formed (balanced `<li>`/`</li>`, `&lt;`/`&gt;` escapes).

- [ ] **Step 6: Commit**

```bash
git add docs/architecture.html docs/architecture.en.html
git commit -m "docs: secret masking is job-wide and exact; short values damage output"
```

### Task 2: READMEs

**Files:**
- Modify: `README.md` (paragraph at line ~447 ending "Secret values, join codes and tokens are masked in logs and output.")
- Modify: `README.ru.md` (paragraph at line ~450 ending "Значения секретов, коды вступления и токены маскируются в логах и выводе.")

- [ ] **Step 1: Append to the English paragraph in `README.md`**

Sentence appended after the existing last sentence:

```
Masking is exact and job-wide: the exact value of every secret of a job is replaced with `***` in the output of all its steps, with no minimum length, so a short or common value (`true`, `1`, `K`) would also damage ordinary output; use long random values for secrets.
```

- [ ] **Step 2: Append to the Russian paragraph in `README.ru.md`**

```
Маскировка точная и действует на всю джобу: точное значение каждого секрета джобы заменяется на `***` в выводе всех её шагов, без минимальной длины, поэтому короткое или распространённое значение (`true`, `1`, `K`) испортит и обычный вывод; для секретов нужны длинные случайные значения.
```

- [ ] **Step 3: Verify**

Run: `grep -n "exact and job-wide" README.md; grep -n "точная и действует на всю джобу" README.ru.md; wc -l README.md README.ru.md`
Expected: one hit each; both files under 500 lines is not required if they already exceed it (the change adds no new lines).

- [ ] **Step 4: Commit**

```bash
git add README.md README.ru.md
git commit -m "docs: README: secret masking scope"
```

### Task 3: Confirm nothing else needs changing

**Files:** none modified unless a check fails.

- [ ] **Step 1: Check plugin skills and `profile check` text**

Run: `grep -rn -i -E "mask|secret" plugin crates/lr-cli/src/commands/profile_cmd.rs | grep -i mask`
Expected: no output. `plugin/skills/*` and the `profile check` output do not describe masking, so they stay unchanged. Any hit that claims per-step or length-based masking must be fixed in the same pair-sync manner.

- [ ] **Step 2: Confirm the `mask_secrets` doc comment still matches**

Read `crates/lr-abstractions/src/process.rs:428-431`; it says every occurrence of each non-empty secret value is replaced, which matches the decision. No edit.

- [ ] **Step 3: Confirm the diff is docs only**

Run: `git diff --stat origin/main...HEAD`
Expected: only `docs/architecture.html`, `docs/architecture.en.html`, `README.md`, `README.ru.md` (plus this plan file, if not yet committed); no `crates/` change.

- [ ] **Step 4: Commit the plan file**

The plan is untracked (earlier plans in `docs/superpowers/plans/` are tracked). Run: `git add docs/superpowers/plans/2026-11-14-secret-masking-docs.md && git commit -m "docs: plan for secret masking scope documentation"`

- [ ] **Step 5: Push and open the PR (when the owner asks)**

PR title: `docs: secret masking is job-wide and exact`. Body in English, states that behavior is unchanged by decision and ends with `Closes #74`; no attribution lines.
