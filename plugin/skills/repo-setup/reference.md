# repo-setup reference

Fields the skill may propose. `localrunner profile check` has the last word.

## Profiles file `.localrunner/profiles.toml`

Only `[profile.<name>]` and `[needs.<name>]` are read at the top level; any other section is ignored and `profile check` warns about it (`[profiles.x]` is a typo).

`[profile.<name>]` keys:
- `command` (one step) or `steps` (an ordered list), never both.
- `timeout_s`: required, no default.
- `mem_mb`, `cpu`, `disk_mb`: what the job holds while it runs.
- `platforms`: a list of platform names; `platform.<P>` tables override `command`, `steps`, `timeout_s`, `mem_mb`, `cpu`, `disk_mb` for one platform.
- `programs`: names, or `{ name = "...", check = "..." }`. List every program the command or a step runs (`python`, `git`, `node`): nothing reads the command text, so a profile without `programs` can land on a runner that does not have the program and fails there in milliseconds.
- `tags`: resources held for the run; `exclusive`: tags nobody else may hold meanwhile.
- `labels`: free-form names the broker policy can match (a rule with `label`); not requirements, so no runner has to declare them. Only the machine owner gives priority by a label, in `policy.toml`.
- `cache`: named caches kept on the runner between runs.
- `services`: runner services the job needs.
- `secrets`: secret names, never values.
- `zone`: the farthest allowed runner zone (`local`, `shared`, `lan`, `remote`); `zones = ["local", "shared"]`: the allowed zones as a list (it also turns on steps per zone).
- `[profile.<name>.zone.<zone>]` with `command` or `steps`: the steps of that zone, replacing the common ones there and opening it (`shared` is a disk the broker owner lists in `[[shared]]`; `lan` and `remote` get code only from a variant of their own). Variants keep the step names of the common steps. `[profile.<name>.variant.<v>]` with `when = { zone = [...], os = [...], tags = [...], programs = [...], runner = [...] }` and `command` or `steps` picks steps by several properties of the runner at once (each key a list, any value; all keys must match; the first match in name order wins, no match keeps the zone or common steps): use it when one zone holds Windows and Linux runners that need different commands. `profile check` prints the zone variants and the variants with their filters. An order picks zones with `--zones local,shared` (or `--max-zone shared`); a branch or commit is an ordinary `--arg`.
- `needs`: the name of a `[needs.<name>]` set the profile inherits from, with field overrides.
- `args.<name>`: a named argument filled with `--arg name=value`; keys `one_of` or `pattern`, `default`, `many`, `join`. A command uses `{name}`; `{? text {name}}` is an optional segment. Spaces at the edges of a segment are dropped, so put the separator outside it: `--no-fail-fast {? -p {pkg}}`, not `--no-fail-fast{? -p {pkg}}` (that glues to `--no-fail-fast-p core`). `profile check` prints the command expanded with every optional argument given and with none given; read it after writing a template.

Example of one profile for the whole set and for a part of it:

```toml
[profile.test]
command   = "cargo test --no-fail-fast {? -p {pkg}} {? {filter}}"
timeout_s = 1800
mem_mb    = 8192
cpu       = 4

[profile.test.args.pkg]
pattern = "[a-z0-9_-]+"

[profile.test.args.filter]
pattern = "[A-Za-z0-9_:]+"
```

`localrunner run test --as <label>` runs everything, `localrunner run test --arg pkg=core --arg filter=parse --as <label>` one package and one name filter. An argument without a `default` and outside `{? ...}` is required.

Step keys: `name`, `command`, `timeout_s`, `continue_on_failure`; also `mem_mb`, `cpu`, `tags`, `secrets`.

`[needs.<name>]` takes the shared requirement keys: `timeout_s`, `mem_mb`, `cpu`, `disk_mb`, `programs`, `tags`, `exclusive`, `cache`, `services`, `secrets`, `zone`, `zones`, `platforms`.

There is no `env`, no working directory, no `matrix` and no retry. Several commands in one environment are `steps` or one script called from `command`.

## Pitfalls

1. A profile with `platforms` or a `platform.*` table needs a runner that declares the platform: `localrunner config set runner:<name> platforms '["<platform>"]'` (the template has `platforms` commented out). A profile without `[profile.X.zone.<zone>]` variants runs only in the zones `local` and `shared`; to run on a `lan` or `remote` runner give it a variant for that zone, whose steps bring the code (for example `git fetch` and `checkout` of `{branch}`, passed as `--arg branch=...`).
2. In `lan` and `remote` a job works in its own folder, which the runner deletes when the job ends: what must survive between runs (a dependency store, a build cache) is declared as a `cache` of the profile. In `local` and `shared` the job works in the client's directory.
3. A `keychain:` secret source is parsed but not read by `profile check`; prefer `file:` (made by `localrunner secret set`; it survives restarts and works for a runner that is an OS service; `profile check` only tells whether the file exists), `env:` only for a one-off run.
3. The secret names `PATH`, `HOME`, `TEMP`, `TMP` and `LOCALRUNNER_*` are reserved and fail the job.
4. The `check` of a profile's `programs` runs only on the runner, against its own `[programs]`.
5. Masking of secrets in output is job-wide and exact: never use short values such as `true` or `1` as secrets.

## Machine settings the skill may propose

Each is one command, run first with `--dry-run` (step 7 of the skill).

Runner keys, `localrunner config set runner:<name> <key> <value>` (the value is TOML; other text is a string):
- `platforms`, `tags`: a list such as `'["a", "b"]'`; it replaces the whole list, so read it first with `localrunner config get runner:<name> tags`.
- `programs.<name>`: `{}` or `'{ check = "..." }'`.
- `secret.<name>.from`: `env:VAR`, or `file:/absolute/path` (a file with the value, one trailing line break dropped); a runner that becomes an OS service does not see your environment, so give it `file:`. An `env:` variable that only your shell has is lost when the runner is restarted from another shell (`upgrade` restarts a runner through its systemd user unit when `autostart on` made one, so an `EnvironmentFile` of the unit applies); the runner warns at start about a secret whose variable is unset or whose file is missing, and so do `doctor` and `upgrade` (before it restarts the runner). `localrunner secret set|list|remove <runner> ...` manages the `file:` form for you.
- `job_env.<NAME>`: a value; `job_env.inherit`: a list of exact names copied from the runner process at the start of each job (`'["ProgramFiles(x86)"]'`).
- `service.<name>.<key>` with the keys `start`, `ready_check`, `stop`, `ready_timeout_s`, `memory_mb`, `lifetime`, `idle_timeout_s`, `max_lifetime_s`, `reuse`.

Broker policy: `localrunner policy allow-secret <name> --label <label>` allows a secret for a client label (`[[secrets.allow]]`), `localrunner policy allow-service <name> --label <label>` a service (`[[services.allow]]`); `remove-secret` and `remove-service` take one back.

`localrunner config show runner:<name>` prints the path and the text of the file a runner reads; `localrunner config show broker` the same for the broker policy.

Never propose `allow_elevated` or `allow_same_user_as_broker`.
