**Added:** profile variants: `[profile.X.variant.NAME]` with `when = { zone, os, tags, programs, runner }`. The runner takes the first variant (in name order) that fits it, and a zone opened only by variants takes only runners a variant fits. `profile check` warns about a variant that an earlier one hides, variants that can match the same runner and a variant no runner of this machine matches. A profiles file with `variant` is refused by 0.11.0 and older.

**Added:** `localrunner secret set|list|remove`: the value comes from stdin or `--value-file`, never from the command line, is stored in a private file and `[secret.NAME]` points at it. `doctor` checks the sources of the secrets, `upgrade` warns before the restart about `env:` secrets whose variable is missing.

**Added:** `wait --broker`; `autostart status [--json]`; `down --wait`; a ZONE column in `runners`; zone, priority and rule of a run are stored (broker.db v6) and shown by `history`, `explain` and the dashboard; `explain` of a waiting order lists why the other runners do not suit it; refusals name the client label and the reasons of every broker.

**Changed:** `upgrade` asks for one administrator request, never downgrades the service, restarts after a failed copy and restarts roles through their systemd or launchd unit. Administrator commands of a broker service run as `sudo -u al-localrunner <service binary>`; a second user broker beside the service is refused and the old broker files are listed. `--background` releases the standard streams; a refused connection to the broker names the command that starts it.

**Changed:** the skills and both READMEs describe the above; the dashboard history grid has the columns zone, priority and broker.
