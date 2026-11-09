# Al.LocalRunner — local queued runners

## Project overview
See `README.md` and `docs/design.md`.

## Rules
- Respond in Russian; write AGENTS.md and all AI instructions in English
- Do not add comments to code unless requested, except doc comments on public items
- Do not create files unless strictly necessary; read a file before modifying it
- Keep files under 500 lines; decompose by responsibility
- Save temporary files to `temp/` (gitignored)
- Never read, print or copy files under `secrets/`
- No agent attribution (`Co-Authored-By`, "Generated with") in commits and pull requests
- Plugins are installed at project scope only (`--scope project`)
- Only third-party libraries with a free commercial-use license (MIT, Apache 2.0, BSD, ISC; MPL-2.0 unmodified)
- **Generic core.** The daemon, queue and scheduler MUST NOT name or branch on a specific consumer, language, toolchain or test framework. All specificity comes from consumer-supplied profiles (`.localrunner/profiles.toml`)
- Diagrams in Markdown files use Mermaid
- Follow the `development-standards` plugin for process phases (enabled in `.claude/settings.json`)
