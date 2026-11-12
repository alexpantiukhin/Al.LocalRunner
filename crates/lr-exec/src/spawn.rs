use crate::mask::Masker;
use lr_abstractions::{ResourceLimits, SecretValue};
use std::io;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[cfg(unix)]
pub(crate) use crate::tree_unix::{Placement, Tree};
#[cfg(windows)]
pub(crate) use crate::tree_windows::{Placement, Tree};

/// Largest piece of output passed to a [`lr_abstractions::JobOutput`] in one event when the
/// job has no secrets; masking can grow a piece by the length of the mask per hidden value.
pub const MAX_CHUNK: usize = 8192;

const CHANNEL: usize = 16;

pub(crate) struct Spawned {
    pub(crate) child: Child,
    pub(crate) tree: Tree,
    pub(crate) output: mpsc::Receiver<Vec<u8>>,
    pub(crate) readers: Vec<JoinHandle<()>>,
}

pub(crate) struct Launch<'a> {
    pub(crate) command: &'a str,
    pub(crate) cwd: &'a Path,
    pub(crate) env: &'a [(String, String)],
    pub(crate) secrets: &'a [(String, SecretValue)],
    pub(crate) limits: &'a ResourceLimits,
}

#[cfg(windows)]
const OS_ENV: [&str; 4] = ["SystemRoot", "windir", "ComSpec", "PATHEXT"];

#[cfg(windows)]
fn os_env(given: &[(String, String)]) -> Vec<(String, String)> {
    OS_ENV
        .iter()
        .filter(|n| !given.iter().any(|(k, _)| k.eq_ignore_ascii_case(n)))
        .filter_map(|n| std::env::var(n).ok().map(|v| (n.to_string(), v)))
        .collect()
}

#[cfg(not(windows))]
fn os_env(_given: &[(String, String)]) -> Vec<(String, String)> {
    Vec::new()
}

fn command(launch: &Launch<'_>) -> (Command, Placement) {
    let names: Vec<&str> = launch
        .env
        .iter()
        .map(|(k, _)| k.as_str())
        .chain(launch.secrets.iter().map(|(k, _)| k.as_str()))
        .collect();
    let (mut cmd, extra, placement) = Tree::shell(launch.command, &names, launch.limits);
    cmd.current_dir(launch.cwd)
        .env_clear()
        .envs(os_env(launch.env))
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .envs(launch.secrets.iter().map(|(k, v)| (k, v.expose())))
        .envs(extra)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    Tree::prepare(&mut cmd, launch.limits, &placement);
    (cmd, placement)
}

fn start(
    mut cmd: Command,
    placement: Placement,
    limits: &ResourceLimits,
) -> io::Result<(Child, Tree)> {
    if cmd.as_std().get_current_dir().is_some_and(|d| !d.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "working directory does not exist",
        ));
    }
    let mut child = cmd.spawn()?;
    match Tree::attach(&child, placement, limits) {
        Ok(tree) => Ok((child, tree)),
        Err(err) => {
            let _ = child.start_kill();
            Err(err)
        }
    }
}

pub(crate) fn spawn_step(launch: &Launch<'_>, mask: &[&SecretValue]) -> io::Result<Spawned> {
    let (mut cmd, placement) = command(launch);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let (mut child, tree) = start(cmd, placement, launch.limits)?;
    let (tx, rx) = mpsc::channel(CHANNEL);
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(forward(stdout, Masker::new(mask), tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(forward(stderr, Masker::new(mask), tx));
    }
    Ok(Spawned {
        child,
        tree,
        output: rx,
        readers,
    })
}

pub(crate) fn spawn_service(launch: &Launch<'_>) -> io::Result<(Child, Tree)> {
    let (mut cmd, placement) = command(launch);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    start(cmd, placement, launch.limits)
}

fn forward<R>(mut stream: R, mut masker: Masker, out: mpsc::Sender<Vec<u8>>) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_CHUNK];
        let mut open = true;
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let rest = masker.finish();
                    if open && !rest.is_empty() {
                        let _ = out.send(rest).await;
                    }
                    break;
                }
                Ok(n) => {
                    let piece = masker.push(&buf[..n]);
                    if open && !piece.is_empty() && out.send(piece).await.is_err() {
                        open = false;
                    }
                }
            }
        }
    })
}
