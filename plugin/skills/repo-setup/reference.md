# repo-setup reference

Fields the skill may propose. `localrunner profile check` has the last word.

## Profiles file `.localrunner/profiles.toml`

Only `[profile.<name>]` and `[needs.<name>]` are read at the top level; any other section is ignored and `profile check` warns about it (`[profiles.x]` is a typo).

`[profile.<name>]` keys:
- `command` (one step) or `steps` (an ordered list), never both.
- `timeout_s`: required, no default.
- `mem_mb`, `cpu`, `disk_mb`: what the job holds while it runs.
- `platforms`: a list of platform names; `platform.<P>` tables override `command`, `steps`, `timeout_s`, `mem_mb`, `cpu`, `disk_mb` for one platform.
- `programs`: names, or `{ name = "...", check = "..." }`.
- `tags`: resources held for the run; `exclusive`: tags nobody else may hold meanwhile.
- `labels`: free-form names the broker policy can match (a rule with `label`); not requirements, so no runner has to declare them. Only the machine owner gives priority by a label, in `policy.toml`.
- `cache`: named caches kept on the runner between runs.
- `services`: runner services the job needs.
- `secrets`: secret names, never values.
- `zone`: the farthest allowed runner zone (`local`, `shared`, `lan`, `remote`); `zones = ["local", "shared"]`: the allowed zones as a list (it also turns on steps per zone).
- `[profile.<name>.zone.<zone>]` with `command` or `steps`: the steps of that zone, replacing the common ones there and opening it (`shared` is a disk the broker owner lists in `[[shared]]`; `lan` and `remote` get code only from a variant of their own). Variants keep the step names of the common steps. An order picks zones with `--zones local,shared` (or `--max-zone shared`); a branch or commit is an ordinary `--arg`.
- `needs`: the name of a `[needs.<name>]` set the profile inherits from, with field overrides.
- `args.<name>`: a named argument filled with `--arg name=value`; keys `one_of` or `pattern`, `default`, `many`, `join`. A command uses `{name}`; `{? text {name}}` is an optional segment.

Step keys: `name`, `command`, `timeout_s`, `continue_on_failure`; also `mem_mb`, `cpu`, `tags`, `secrets`.

`[needs.<name>]` takes the shared requirement keys: `timeout_s`, `mem_mb`, `cpu`, `disk_mb`, `programs`, `tags`, `exclusive`, `cache`, `services`, `secrets`, `zone`, `zones`, `platforms`.

There is no `env`, no working directory, no `matrix` and no retry. Several commands in one environment are `steps` or one script called from `command`.

## Pitfalls

1. A profile with `platforms` or a `platform.*` table needs a runner that declares the platform in its `runner.toml`, where the template has `platforms` commented out. A profile without `[profile.X.zone.<zone>]` variants runs only in the zones `local` and `shared`; to run on a `lan` or `remote` runner give it a variant for that zone, whose steps bring the code (for example `git fetch` and `checkout` of `{branch}`, passed as `--arg branch=...`).
2. A `keychain:` secret source is parsed but not read by `profile check`; prefer `env:`.
3. The secret names `PATH`, `HOME`, `TEMP`, `TMP` and `LOCALRUNNER_*` are reserved and fail the job.
4. The `check` of a profile's `programs` runs only on the runner, against its own `[programs]`.
5. Masking of secrets in output is job-wide and exact: never use short values such as `true` or `1` as secrets.

## Machine files the skill may propose

`runner.toml`: `platforms`, `tags`, `[programs]` (`name = {}` or `{ check = "..." }`), `[secret.<name>]` with `from = "env:VAR"`, `[service.<name>]` with `start`, `ready_check`, `stop`, `ready_timeout_s`, `memory_mb`, `lifetime`, `idle_timeout_s`, `max_lifetime_s`, `reuse`.

`policy.toml`: `[[secrets.allow]]` with `label` and `names`; `[[services.allow]]` with `label` and `names`.

Never propose `allow_elevated` or `allow_same_user_as_broker`.
