**Deprecated:** the `[workspace]` section of `runner.toml` has had no effect since 0.7.0. It is still read without an error; `doctor` and the runner start report `deprecated since 0.7.0, remove it`. Delete the section by hand.

**Added:** `from = "file:PATH"` as a secret source of `runner.toml`, the way for an OS service runner (an `env:` secret is invisible to a service). `autostart on runner <name> --service` gives the Windows service account read access to the file, and prints the service notes and the `runner.toml` checks before the rights are asked for and in `--dry-run`.
