//! `git` commands of the identity facts: run as a child process without a shell, with the
//! repository overrides of the caller's environment removed (spec 13.3, 13.4).

use std::path::Path;
use std::process::Command;

/// `url` without credentials: an `http(s)` URL loses its whole userinfo (`user:TOKEN@`,
/// `TOKEN@`), any other `scheme://` URL keeps only the user name; scp-like addresses
/// (`git@host:path`) carry no secret and stay as they are. The origin travels to the broker
/// and to runners of other machines, so a token must not.
pub fn without_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    let web = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    match userinfo.split_once(':').map(|(u, _)| u).or(Some(userinfo)) {
        Some(user) if !web && !user.is_empty() => format!("{scheme}://{user}@{host}{tail}"),
        _ => format!("{scheme}://{host}{tail}"),
    }
}

/// A `git -C <cwd>` command that ignores the caller's repository overrides: every `GIT_*`
/// variable of `env` (any case) is removed and `GIT_TERMINAL_PROMPT=0` set, so neither
/// `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_CONFIG_*` nor the like can point git elsewhere.
pub fn git_command_in<I>(cwd: &Path, env: I) -> Command
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut cmd = Command::new("git");
    for var in env {
        if var
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            cmd.env_remove(var);
        }
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0").arg("-C").arg(cwd);
    cmd
}

/// [`git_command_in`] over the environment of this process.
pub fn git_command(cwd: &Path) -> Command {
    git_command_in(cwd, std::env::vars_os().map(|(k, _)| k))
}
