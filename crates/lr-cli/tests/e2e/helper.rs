use crate::support::HANG;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

const MODE: &str = "LR_E2E_MODE";
const ROLE: &str = "LR_E2E_ROLE";
const NAME: &str = "LR_E2E_NAME";
const LINK: &str = "LR_E2E_LINK";
const READY: &str = "LR_E2E_READY";
const EXEC: &str = "LR_E2E_EXEC";
const SEPARATOR: char = '\u{1f}';
const TEST_NAME: &str = "helper_process";

pub enum Mode {
    Tree,
    Wait,
}

/// A helper process that reported over its link: its pid and the link itself, whose end
/// means the process is gone.
struct Arrival {
    name: String,
    role: String,
    pid: u32,
    link: TcpStream,
    #[cfg(windows)]
    process: Option<Process>,
}

/// The loopback listener the helpers of a test connect to. Each helper reports `name role
/// pid` and then waits for one byte (the gate opening) or the end of the link; the link ends
/// when the helper's process ends.
pub struct Gate {
    addr: String,
    arrivals: Arc<Mutex<Vec<Arrival>>>,
    changes: watch::Receiver<usize>,
    accept: tokio::task::JoinHandle<()>,
}

impl Gate {
    pub async fn new() -> Arc<Gate> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let arrivals = Arc::new(Mutex::new(Vec::new()));
        let (tx, changes) = watch::channel(0usize);
        let seen = arrivals.clone();
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = seen.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Some(a) = arrive(stream).await {
                        seen.lock().unwrap().push(a);
                        tx.send_modify(|n| *n += 1);
                    }
                });
            }
        });
        Arc::new(Gate {
            addr,
            arrivals,
            changes,
            accept,
        })
    }

    /// The command of a helper `name` linked to this gate.
    pub fn command(&self, mode: Mode, name: &str) -> String {
        self.command_with(mode, name, None)
    }

    /// [`Gate::command`] whose helper also creates file `ready` once it runs.
    pub fn command_with(&self, mode: Mode, name: &str, ready: Option<&Path>) -> String {
        let exe = std::env::current_exe().unwrap().display().to_string();
        let args = format!("--exact {TEST_NAME} --nocapture --quiet --test-threads=1");
        let mode = match mode {
            Mode::Tree => "tree",
            Mode::Wait => "wait",
        };
        let mut vars = vec![
            (MODE, mode.to_string()),
            (ROLE, "child".to_string()),
            (NAME, name.to_string()),
            (LINK, self.addr.clone()),
        ];
        if let Some(r) = ready {
            vars.push((READY, r.display().to_string()));
        }
        if cfg!(windows) {
            let sets: String = vars
                .iter()
                .map(|(k, v)| format!("set \"{k}={v}\"&& "))
                .collect();
            format!("{sets}\"{exe}\" {args}")
        } else {
            let sets: String = vars.iter().map(|(k, v)| format!("{k}='{v}' ")).collect();
            format!("{sets}'{exe}' {args}")
        }
    }

    /// The helper `role` of `name`, once it has reported.
    pub async fn started(&self, name: &str, role: &str) -> Helper {
        let mut changes = self.changes.clone();
        let found = async {
            loop {
                let found = self
                    .arrivals
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|a| a.name == name && a.role == role)
                    .map(|a| Helper {
                        pid: a.pid,
                        link: a.link.try_clone().unwrap(),
                    });
                if let Some(h) = found {
                    return h;
                }
                changes.changed().await.unwrap();
            }
        };
        tokio::time::timeout(HANG, found)
            .await
            .unwrap_or_else(|_| panic!("the {role} helper of {name} never reported"))
    }

    /// Lets every helper that has reported so far end.
    pub fn open(&self) {
        for a in self.arrivals.lock().unwrap().iter_mut() {
            let _ = a.link.write_all(b"o");
        }
    }

    /// Opens the gate and waits until every helper that reported has ended.
    fn release(&self) {
        self.open();
        self.accept.abort();
        for a in self.arrivals.lock().unwrap().drain(..) {
            let _ = ended(&a.link);
            #[cfg(windows)]
            if let Some(p) = a.process {
                p.wait();
            }
        }
    }
}

async fn arrive(stream: tokio::net::TcpStream) -> Option<Arrival> {
    let std = stream.into_std().ok()?;
    std.set_nonblocking(false).ok()?;
    tokio::task::spawn_blocking(move || {
        std.set_read_timeout(Some(HANG)).ok()?;
        let mut line = String::new();
        let mut reader = BufReader::new(std.try_clone().ok()?);
        reader.read_line(&mut line).ok()?;
        std.set_read_timeout(None).ok()?;
        let mut words = line.split_whitespace();
        let name = words.next()?.to_string();
        let role = words.next()?.to_string();
        let pid: u32 = words.next()?.parse().ok()?;
        Some(Arrival {
            name,
            role,
            pid,
            link: std,
            #[cfg(windows)]
            process: Process::open(pid),
        })
    })
    .await
    .ok()?
}

/// Blocks until the link ends (the process at its other end is gone), at most [`HANG`].
fn ended(link: &TcpStream) -> std::io::Result<()> {
    let mut link = link.try_clone()?;
    link.set_read_timeout(Some(HANG))?;
    let mut buf = [0u8; 16];
    loop {
        match link.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                return Ok(())
            }
            Err(e) => return Err(e),
        }
    }
}

/// A helper process that reported over its link.
pub struct Helper {
    pub pid: u32,
    link: TcpStream,
}

impl Helper {
    /// The pid and start time of the helper.
    pub fn process(&self) -> (u32, u64) {
        (
            self.pid,
            lr_exec::process_start(self.pid).expect("the helper runs"),
        )
    }

    /// Waits until the helper's process is gone.
    pub async fn gone(&self, what: &str) {
        let link = self.link.try_clone().unwrap();
        let ended = tokio::task::spawn_blocking(move || ended(&link))
            .await
            .unwrap();
        assert!(ended.is_ok(), "{what}: {ended:?}");
    }
}

/// Opens the gates when the test ends, even by a panic, and waits until every helper that
/// reported to them has ended, so none of them holds the temporary directory.
pub struct Release(Vec<Arc<Gate>>);

impl Release {
    pub fn new(gates: &[&Arc<Gate>]) -> Release {
        Release(gates.iter().map(|g| Arc::clone(g)).collect())
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        for g in &self.0 {
            g.release();
        }
    }
}

#[cfg(windows)]
struct Process(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for Process {}

#[cfg(windows)]
impl Process {
    fn open(pid: u32) -> Option<Process> {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        (!h.is_null()).then_some(Process(h))
    }

    fn wait(self) {
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        unsafe { WaitForSingleObject(self.0, HANG.as_millis() as u32) };
    }
}

#[cfg(windows)]
impl Drop for Process {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

fn report(role: &str) -> TcpStream {
    let addr = std::env::var(LINK).unwrap();
    let name = std::env::var(NAME).unwrap();
    let mut link = TcpStream::connect(addr).unwrap();
    writeln!(link, "{name} {role} {}", std::process::id()).unwrap();
    if let Ok(ready) = std::env::var(READY) {
        std::fs::write(ready, b"ready").unwrap();
    }
    link
}

fn spawn_grandchild() {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST_NAME, "--nocapture", "--quiet"])
        .env(MODE, "wait")
        .env(ROLE, "grandchild")
        .env_remove(READY)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut cmd);
    std::mem::forget(cmd.spawn().unwrap());
}

#[cfg(windows)]
fn detach(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

pub fn run_if_requested() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    match mode.as_str() {
        "exec" => run_step(),
        "sender" => {
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            std::process::exit(0);
        }
        "tree" => spawn_grandchild(),
        _ => {}
    }
    let role = std::env::var(ROLE).unwrap();
    let mut link = report(&role);
    println!("{role}-started");
    let _ = link.read(&mut [0u8; 1]);
    println!("{role}-done");
    std::process::exit(0);
}

/// A process standing for a sender: it lives until it is killed or its stdin closes.
pub fn spawn_sender() -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture", "--quiet"])
        .env(MODE, "sender")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap()
}

/// A process standing for a CI step: it runs `program` with `args` in `cwd` (with the
/// environment `envs` added) and ends with the program. The step and the program share the
/// write end of the step's stderr, which the test drains: it ends with the last of them.
pub struct Step {
    pub child: std::process::Child,
    drained: tokio::task::JoinHandle<()>,
}

impl Step {
    pub fn spawn(program: &Path, args: &[String], cwd: &Path, envs: &[(&str, &str)]) -> Step {
        let line = std::iter::once(program.display().to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(&SEPARATOR.to_string());
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", TEST_NAME, "--nocapture", "--quiet"])
            .env(MODE, "exec")
            .env(EXEC, line)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let drained = tokio::task::spawn_blocking(move || {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        });
        Step { child, drained }
    }

    /// Waits until the step and its program are gone.
    pub async fn gone(self, what: &str) {
        tokio::time::timeout(HANG, self.drained)
            .await
            .unwrap_or_else(|_| panic!("{what}"))
            .unwrap();
    }
}

fn run_step() {
    let line = std::env::var(EXEC).unwrap();
    let mut words = line.split(SEPARATOR);
    let program = words.next().unwrap().to_string();
    let mut child = std::process::Command::new(program)
        .args(words)
        .env_remove(MODE)
        .env_remove(EXEC)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let _ = child.wait();
    std::process::exit(0);
}
