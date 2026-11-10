use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const MODE: &str = "LR_EXEC_HELPER";
const ROLE: &str = "LR_EXEC_ROLE";
const MARKER: &str = "LR_EXEC_MARKER";
const MARKER2: &str = "LR_EXEC_MARKER2";
const GATE: &str = "LR_EXEC_GATE";
const COUNT: &str = "LR_EXEC_COUNT";
const DETACH: &str = "LR_EXEC_DETACH";
const PIDFILE: &str = "LR_EXEC_PIDFILE";
const READY: &str = "LR_EXEC_READY";
const TEST_NAME: &str = "common::helper_process";

static SOFT: AtomicBool = AtomicBool::new(false);
static SOFT_MARKER: OnceLock<std::ffi::CString> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct Helper {
    pub mode: &'static str,
    pub marker: Option<PathBuf>,
    pub marker2: Option<PathBuf>,
    pub gate: Option<PathBuf>,
    pub count: Option<u64>,
    pub detach: bool,
    pub pidfile: Option<PathBuf>,
}

impl Helper {
    pub fn new(mode: &'static str) -> Helper {
        Helper {
            mode,
            detach: true,
            ..Helper::default()
        }
    }

    pub fn marker(mut self, path: &Path) -> Helper {
        self.marker = Some(path.to_path_buf());
        self
    }

    pub fn marker2(mut self, path: &Path) -> Helper {
        self.marker2 = Some(path.to_path_buf());
        self
    }

    pub fn gate(mut self, path: &Path) -> Helper {
        self.gate = Some(path.to_path_buf());
        self
    }

    pub fn count(mut self, n: u64) -> Helper {
        self.count = Some(n);
        self
    }

    pub fn in_group(mut self) -> Helper {
        self.detach = false;
        self
    }

    pub fn pidfile(mut self, path: &Path) -> Helper {
        self.pidfile = Some(path.to_path_buf());
        self
    }

    fn vars(&self) -> Vec<(&'static str, String)> {
        let mut vars = vec![(MODE, self.mode.to_string()), (ROLE, "child".to_string())];
        let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
        for (name, value) in [
            (MARKER, path(&self.marker)),
            (MARKER2, path(&self.marker2)),
            (GATE, path(&self.gate)),
            (PIDFILE, path(&self.pidfile)),
            (COUNT, self.count.map(|n| n.to_string())),
        ] {
            if let Some(value) = value {
                vars.push((name, value));
            }
        }
        vars.push((DETACH, if self.detach { "1" } else { "0" }.to_string()));
        vars
    }
}

pub fn helper_command(helper: &Helper) -> String {
    let exe = std::env::current_exe().unwrap().display().to_string();
    let args = format!("--exact {TEST_NAME} --nocapture --quiet --test-threads=1");
    let vars = helper.vars();
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

pub fn helper_process(helper: &Helper) -> std::process::Command {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST_NAME, "--nocapture", "--quiet"]);
    cmd.envs(helper.vars());
    cmd
}

pub fn run_if_requested() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let role = std::env::var(ROLE).unwrap_or_else(|_| "child".to_string());
    if let Ok(path) = std::env::var(if role == "grandchild" {
        MARKER2
    } else {
        MARKER
    }) {
        let _ = SOFT_MARKER.set(std::ffi::CString::new(path).unwrap());
    }
    install_soft_handler();
    if let Ok(path) = std::env::var(READY) {
        let _ = std::fs::write(path, b"ready");
    }
    match mode.as_str() {
        "ignore" => {
            announce(&role);
            ignore_loop(&role);
        }
        "obey" => {
            announce(&role);
            wait_soft(Duration::from_secs(120));
            say(&format!("{role}-bye\n"));
            std::process::exit(0);
        }
        "tree" => {
            spawn_grandchild();
            announce(&role);
            ignore_loop(&role);
        }
        "spawn-exit" => {
            spawn_grandchild();
            announce(&role);
            std::process::exit(0);
        }
        "wait-file" => {
            announce(&role);
            wait_gate();
            let code = std::env::var(COUNT).ok().and_then(|c| c.parse().ok());
            std::process::exit(code.unwrap_or(0));
        }
        "print-start" => {
            let start = lr_exec::process_start(std::process::id());
            say(&format!("start={start:?} start-end\n"));
            wait_gate();
            std::process::exit(0);
        }
        "no-console-cancel" => super::no_console::run(),
        "big" => {
            let mut out = std::io::stdout().lock();
            out.write_all(&[0xff, 0xfe]).unwrap();
            let block = vec![b'a'; 64 * 1024];
            for _ in 0..160 {
                out.write_all(&block).unwrap();
            }
            out.flush().unwrap();
            std::process::exit(0);
        }
        "split" => {
            say("pre SEC");
            std::thread::sleep(Duration::from_millis(300));
            say("RET post\n");
            std::process::exit(0);
        }
        "flood" => {
            let n: u64 = std::env::var(COUNT).unwrap().parse().unwrap();
            let mut out = std::io::BufWriter::new(std::io::stdout().lock());
            for i in 1..=n {
                if writeln!(out, "0123456789012345678901234567890123456789 {i}").is_err() {
                    std::process::exit(1);
                }
            }
            out.flush().unwrap();
            std::process::exit(0);
        }
        other => panic!("unknown helper mode {other}"),
    }
}

fn wait_gate() {
    let gate = PathBuf::from(std::env::var(GATE).unwrap());
    while !gate.exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn announce(role: &str) {
    let pid = std::process::id();
    if let Ok(path) = std::env::var(PIDFILE) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{role} {pid}").unwrap();
    }
    say(&format!("{role}-pid={pid} {role}-pid-end\n"));
}

fn wait_soft(limit: Duration) {
    let started = Instant::now();
    while !SOFT.load(Ordering::SeqCst) && started.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn ignore_loop(role: &str) {
    let marker = std::env::var(if role == "grandchild" {
        MARKER2
    } else {
        MARKER
    })
    .ok();
    let started = Instant::now();
    let mut written = false;
    while started.elapsed() < Duration::from_secs(120) {
        if SOFT.load(Ordering::SeqCst) && !written {
            written = true;
            if let Some(path) = &marker {
                std::fs::write(path, b"soft").unwrap();
            }
            say(&format!("{role}-got-soft\n"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::process::exit(0);
}

fn spawn_grandchild() {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST_NAME, "--nocapture", "--quiet"]);
    let ready = std::env::temp_dir().join(format!("lr-exec-ready-{}", std::process::id()));
    let _ = std::fs::remove_file(&ready);
    cmd.env(MODE, "ignore")
        .env(ROLE, "grandchild")
        .env(READY, &ready);
    let detach = std::env::var(DETACH).map(|v| v == "1").unwrap_or(true);
    if detach {
        detach_command(&mut cmd);
    }
    let child = cmd.spawn().unwrap();
    let started = Instant::now();
    while !ready.exists() && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(&ready);
    say(&format!(
        "grandchild-pid={} grandchild-pid-end\n",
        child.id()
    ));
    std::mem::forget(child);
}

#[cfg(windows)]
fn detach_command(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn detach_command(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn install_soft_handler() {
    use windows_sys::Win32::Foundation::BOOL;
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    };
    unsafe extern "system" fn handler(kind: u32) -> BOOL {
        if kind == CTRL_BREAK_EVENT || kind == CTRL_C_EVENT {
            if let Some(path) = SOFT_MARKER.get() {
                let _ = std::fs::write(path.to_str().unwrap(), b"soft");
            }
            SOFT.store(true, Ordering::SeqCst);
            1
        } else {
            0
        }
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

#[cfg(unix)]
fn install_soft_handler() {
    extern "C" fn handler(_: libc::c_int) {
        if let Some(path) = SOFT_MARKER.get() {
            unsafe {
                let fd = libc::open(
                    path.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    0o644 as libc::c_uint,
                );
                if fd >= 0 {
                    libc::write(fd, b"soft".as_ptr() as *const libc::c_void, 4);
                    libc::close(fd);
                }
            }
        }
        SOFT.store(true, Ordering::SeqCst);
    }
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}
