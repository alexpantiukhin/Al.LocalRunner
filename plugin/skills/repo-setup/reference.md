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
- `cache`: named caches kept on the runner between runs.
- `services`: runner services the job needs.
- `secrets`: secret names, never values.
- `zone`: the farthest allowed runner zone.
- `needs`: the name of a `[needs.<name>]` set the profile inherits from, with field overrides.
- `args.<name>`: a named argument filled with `--arg name=value`; keys `one_of` or `pattern`, `default`, `many`, `join`. A command uses `{name}`; `{? text {name}}` is an optional segment.

Step keys: `name`, `command`, `timeout_s`, `continue_on_failure`; also `mem_mb`, `cpu`, `tags`, `secrets`.

`[needs.<name>]` takes the shared requirement keys: `timeout_s`, `mem_mb`, `cpu`, `disk_mb`, `programs`, `tags`, `exclusive`, `cache`, `services`, `secrets`, `zone`, `platforms`.

There is no `env`, no working directory, no `matrix` and no retry. Several commands in one environment are `steps` or one script called from `command`.

## Pitfalls

1. A profile with `platforms` or a `platform.*` table is ordered by commit: the order carries a pushed commit (a pushed `HEAD` is required for a runner of another installation) and the runner must declare the platform in its `runner.toml`, where the template has `platforms` commented out. Without platforms the profile runs by work tree.
2. A `keychain:` secret source is parsed but not read by `profile check`; prefer `env:`.
3. The secret names `PATH`, `HOME`, `TEMP`, `TMP` and `LOCALRUNNER_*` are reserved and fail the job.
4. The `check` of a profile's `programs` runs only on the runner, against its own `[programs]`.
5. Masking of secrets in output is job-wide and exact: never use short values such as `true` or `1` as secrets.

## Machine files the skill may propose

`runner.toml`: `platforms`, `tags`, `[programs]` (`name = {}` or `{ check = "..." }`), `[secret.<name>]` with `from = "env:VAR"`, `[service.<name>]` with `start`, `ready_check`, `stop`, `ready_timeout_s`, `memory_mb`, `lifetime`, `idle_timeout_s`, `max_lifetime_s`, `reuse`.

`policy.toml`: `[[secrets.allow]]` with `label` and `names`; `[[services.allow]]` with `label` and `names`.

Never propose `allow_elevated` or `allow_same_user_as_broker`.
