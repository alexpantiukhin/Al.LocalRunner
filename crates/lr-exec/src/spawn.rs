use crate::mask::Masker;
use lr_abstractions::SecretValue;
use std::io;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[cfg(unix)]
pub(crate) use crate::tree_unix::Tree;
#[cfg(windows)]
pub(crate) use crate::tree_windows::Tree;

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
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let mut cmd = Command::new(comspec);
    cmd.raw_arg(format!("/D /S /C \"{command}\""));
    cmd
}

#[cfg(unix)]
fn shell(command: &str) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    cmd
}

fn command(launch: &Launch<'_>) -> Command {
    let mut cmd = shell(launch.command);
    cmd.current_dir(launch.cwd)
        .env_clear()
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .envs(launch.secrets.iter().map(|(k, v)| (k, v.expose())))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    Tree::prepare(&mut cmd);
    cmd
}

fn start(mut cmd: Command) -> io::Result<(Child, Tree)> {
    if cmd.as_std().get_current_dir().is_some_and(|d| !d.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "working directory does not exist",
        ));
    }
    let mut child = cmd.spawn()?;
    match Tree::attach(&child) {
        Ok(tree) => Ok((child, tree)),
        Err(err) => {
            let _ = child.start_kill();
            Err(err)
        }
    }
}

pub(crate) fn spawn_step(launch: &Launch<'_>, mask: &[&SecretValue]) -> io::Result<Spawned> {
    let mut cmd = command(launch);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let (mut child, tree) = start(cmd)?;
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
    let mut cmd = command(launch);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    start(cmd)
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
