//! Broker administration from the CLI: over the broker's local admin channel only
//! (spec 3.9); `policy check` reads the file itself and needs no broker.

use super::confirm::{proceed, Console, Stdio};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_broker::policy::{check, load};
use lr_proto::{AdminReply, AdminRequest};
use std::path::{Path, PathBuf};

/// The file the running broker uses and its warnings, or its refusal; `None` when the admin
/// channel of the broker cannot be reached (no broker, or no right to use the channel).
pub async fn broker_policy(layout: &Layout) -> Option<Result<(String, Vec<String>), String>> {
    let client = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .ok()?;
    let reply = client
        .call::<_, AdminReply>(&AdminRequest::PolicyCheck)
        .await;
    client.close().await;
    Some(match reply {
        Ok(AdminReply::Policy { path, warnings }) => Ok((path, warnings)),
        Ok(AdminReply::Failed { message }) => Err(message),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e),
    })
}

/// The warning for a broker service that cannot be asked: the user's `policy.toml` is not the
/// one the service loads.
pub fn service_policy_note(layout: &Layout) -> Option<String> {
    layout.broker_is_service().then(|| {
        format!(
            "the broker service cannot be asked (it is stopped, or its admin channel is closed to this user), so this is not the file it uses (the service loads {})",
            crate::paths::service_root().join("policy.toml").display()
        )
    })
}

/// `policy check [file]`: errors stop the file, shadowed rules and the like are warnings.
/// Without a file the running broker is asked which `policy.toml` it uses and checks that one.
pub async fn policy_check(layout: &Layout, file: Option<PathBuf>) -> Result<i32, String> {
    if file.is_none() {
        if let Some(answer) = broker_policy(layout).await {
            return match answer {
                Ok((path, warnings)) => {
                    for w in warnings {
                        println!("warning: {w}");
                    }
                    println!("{path}: valid (the file the running broker uses)");
                    Ok(0)
                }
                Err(e) => {
                    eprintln!("{e}");
                    Ok(1)
                }
            };
        }
        if let Some(note) = service_policy_note(layout) {
            println!("warning: {note}");
        }
    }
    let path = file.unwrap_or_else(|| layout.config_file("policy.toml"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    match load(&text) {
        Err(e) => {
            eprintln!("{e}");
            Ok(1)
        }
        Ok(policy) => {
            for w in check(&policy, &crate::host::resources()) {
                println!("warning: {w}");
            }
            println!("{}: valid", path.display());
            Ok(0)
        }
    }
}

/// `runner remove` or `client remove` as the command line runs it (see [`remove_asked`]).
pub async fn remove_cli(layout: &Layout, req: AdminRequest, yes: bool) -> Result<i32, String> {
    let mut stdio = Stdio::new();
    let mut console = stdio.console();
    remove_asked(layout, req, yes, &mut console).await
}

/// Sends the removal `req` (`RunnerRemove` or `ClientRemove`) after asking, because it revokes
/// a key (`yes` answers); a refusal or a "no" ends before the broker is reached.
pub async fn remove_asked(
    layout: &Layout,
    req: AdminRequest,
    yes: bool,
    console: &mut Console<'_>,
) -> Result<i32, String> {
    let (kind, name) = match &req {
        AdminRequest::RunnerRemove { name } => ("runner", name),
        AdminRequest::ClientRemove { name } => ("client", name),
        _ => return simple(layout, req).await,
    };
    let action = format!("{kind} remove revokes the key of {kind} {name}");
    let question = format!("remove {kind} {name} and revoke its key? It needs a new join code.");
    if !proceed(&action, &question, yes, console)? {
        return Ok(1);
    }
    simple(layout, req).await
}

/// `policy reload`: applies the file of the broker and names it.
pub async fn policy_reload(layout: &Layout) -> Result<i32, String> {
    let code = simple(layout, AdminRequest::PolicyReload).await?;
    if code == 0 {
        if let Some(Ok((path, _))) = broker_policy(layout).await {
            println!("policy: {path}");
        }
    }
    Ok(code)
}

/// The error for an admin channel that cannot be opened: a refused access names the cause and
/// what to do about it.
pub fn unreachable_message(cause: &str) -> String {
    let denied = [
        "os error 5",
        "os error 13",
        "ermission denied",
        "ccess is denied",
    ]
    .iter()
    .any(|m| cause.contains(m));
    let base = format!("the broker admin channel is not reachable: {cause}");
    if denied {
        format!(
            "{base}\nthe admin channel is open only to an administrator (Windows) or to the OS user of the broker (Unix): \
             run this command from an elevated shell or as that user. To hand a join code to another user, \
             add --code-file <file> to `broker add` and give the file to `runner join --code-file`"
        )
    } else {
        base
    }
}

/// Writes `code` and a newline to the new file `path`, private to its owner where the OS has
/// such modes; an existing file is never overwritten.
pub fn write_code_file(path: &Path, code: &str) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{}: already exists; give a new file", path.display())
        } else {
            format!("{}: {e}", path.display())
        }
    })?;
    writeln!(file, "{code}").map_err(|e| format!("{}: {e}", path.display()))
}

async fn call(layout: &Layout, req: &AdminRequest) -> Result<AdminReply, String> {
    let client = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .map_err(|e| unreachable_message(&e.to_string()))?;
    let reply = client.call(req).await;
    client.close().await;
    reply
}

/// `broker add`: prints the join code, or with `code_file` writes it to that new file and
/// prints the rest, so the code does not appear on the screen of the administrator.
pub async fn broker_add(
    layout: &Layout,
    req: AdminRequest,
    code_file: Option<PathBuf>,
) -> Result<i32, String> {
    show(call(layout, &req).await?, code_file.as_deref())
}

/// Sends one admin command to the broker of this installation and prints the answer.
pub async fn simple(layout: &Layout, req: AdminRequest) -> Result<i32, String> {
    show(call(layout, &req).await?, None)
}

fn show(reply: AdminReply, code_file: Option<&Path>) -> Result<i32, String> {
    match reply {
        AdminReply::Ok { warnings } => {
            for w in warnings {
                println!("warning: {w}");
            }
            println!("ok");
            Ok(0)
        }
        AdminReply::Policy { path, warnings } => {
            for w in warnings {
                println!("warning: {w}");
            }
            println!("policy: {path}");
            Ok(0)
        }
        AdminReply::JoinCode {
            address,
            code,
            fingerprint,
            expires_at,
        } => {
            println!("address:     {address}");
            match code_file {
                Some(path) => {
                    write_code_file(path, &code)?;
                    println!("code:        written to {}", path.display());
                }
                None => println!("code:        {code}"),
            }
            println!("fingerprint: {fingerprint}");
            println!("expires at:  {} (unix ms)", expires_at.0);
            Ok(0)
        }
        AdminReply::Names { names } => {
            for n in names {
                println!("{n}");
            }
            Ok(0)
        }
        AdminReply::Failed { message } => {
            eprintln!("{message}");
            Ok(1)
        }
    }
}
