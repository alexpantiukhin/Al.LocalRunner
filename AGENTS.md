# Al.LocalRunner — a resource-aware job queue for a developer and AI agents

## Project overview
See `README.md` (English, the main one) and `README.ru.md` (Russian), and `docs/architecture.html` (Russian) with `docs/architecture.en.html` (English): the single design document in two languages (architecture, requirements, config formats, security) with a table of contents. Do not add design Markdown files; update the HTML instead. Keep each pair in sync: a change to one file of a pair is made in the other in the same commit. Each README starts with a language switch line.

## Rules
- Respond in Russian; write AGENTS.md and all AI instructions in English
- Everything on GitHub in this repository is in English: issues, pull requests (titles, descriptions, comments, reviews), commit messages, release notes, labels. Russian and other languages are allowed only in documentation (`README.<lang>.md`, `docs/`) and in instructions for people, and only as a translation in addition to the English original, never instead of it
- Do not add comments to code unless requested, except doc comments on public items
- Do not create files unless strictly necessary; read a file before modifying it
- No limit on file length; still decompose code by responsibility
- Save temporary files to `temp/` (gitignored)
- Run heavy tasks (build, `cargo test`, clippy, the full gate) through `localrunner` using the profiles in `.localrunner/profiles.toml` (`localrunner run rust-unit --as <label>`, `localrunner run rust-gate --as <label>`), not directly, so parallel sessions share the machine. Use the `localrunner:run` skill. There is no direct-run exception: even one test builds the crate and its dependencies on all cores, and a worktree has its own `target`. The rule covers every `cargo build`, `check`, `test`, `clippy` and `doc`, in the main checkout and in worktrees, and it binds subagents too: put it in their prompt, they do not read this file. For a narrower run (one crate, one test) add a profile or a profile argument to `.localrunner/profiles.toml` instead of calling `cargo` by hand. Seeing yourself type `cargo` for any of these is the signal to stop and use `localrunner run`.
- Never read, print or copy files under `secrets/`
- No agent attribution (`Co-Authored-By`, "Generated with") in commits and pull requests
- Plugins are installed at project scope only (`--scope project`)
- **Generic core.** The daemon, queue and scheduler MUST NOT name or branch on a specific consumer, language, toolchain or test framework. All specificity comes from consumer-supplied profiles (`.localrunner/profiles.toml`)
- **Tests do not wait on real time.** Synchronize on an event the code produces (a reply, a queue or SSE event, a closed channel, a marker written after the action); check timers with a manual clock and `start_paused`; do not poll with `eventually` or `sleep` for a result. A timeout is only an upper bound against hangs. If the code has no such signal, add a test hook
- When a path or a directory name used by `install.ps1` / `install.sh` changes, update `plugin/shared/os/*.md` and the `install-runner` skill in the same commit
- Diagrams in Markdown files use Mermaid
