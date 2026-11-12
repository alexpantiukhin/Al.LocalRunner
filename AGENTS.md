# Al.LocalRunner — local queued runners

## Project overview
See `README.md` (English, the main one) and `README.ru.md` (Russian), and `docs/architecture.html` (Russian) with `docs/architecture.en.html` (English): the single design document in two languages (architecture, requirements, config formats, security) with a table of contents. Do not add design Markdown files; update the HTML instead. Keep each pair in sync: a change to one file of a pair is made in the other in the same commit. Each README starts with a language switch line.

## Rules
- Respond in Russian; write AGENTS.md and all AI instructions in English
- Do not add comments to code unless requested, except doc comments on public items
- Do not create files unless strictly necessary; read a file before modifying it
- Keep files under 500 lines; decompose by responsibility (exception: `docs/architecture.html` and `docs/architecture.en.html`, the single design document)
- Save temporary files to `temp/` (gitignored)
- Never read, print or copy files under `secrets/`
- No agent attribution (`Co-Authored-By`, "Generated with") in commits and pull requests
- Plugins are installed at project scope only (`--scope project`)
- Only third-party libraries with a free commercial-use license (MIT, Apache 2.0, BSD, ISC, Zlib, Unicode-3.0; MPL-2.0 unmodified)
- **Generic core.** The daemon, queue and scheduler MUST NOT name or branch on a specific consumer, language, toolchain or test framework. All specificity comes from consumer-supplied profiles (`.localrunner/profiles.toml`)
- Diagrams in Markdown files use Mermaid
- Follow the `development-standards` plugin for process phases (enabled in `.claude/settings.json`)
