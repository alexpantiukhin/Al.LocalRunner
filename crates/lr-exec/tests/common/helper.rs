use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const MODE: &str = "LR_EXEC_HELPER";
const ROLE: &str = "LR_EXEC_ROLE";
const MARKER: &str = "LR_EXEC_MARKER";
const MARKER2: &str = "LR_EXEC_MARKER2";
const LINK: &str = "LR_EXEC_LINK";
const SHOW: &str = "LR_EXEC_SHOW";
const COUNT: &str = "LR_EXEC_COUNT";
const DETACH: &str = "LR_EXEC_DETACH";
const READY: &str = "LR_EXEC_READY";
const TEST_NAME: &str = "common::helper_process";
const LIFETIME: Duration = Duration::from_secs(25);
const READY_LIMIT: Duration = Duration::from_secs(10);

static SOFT_MARKER: OnceLock<std::ffi::CString> = OnceLock::new();
static LINKED: OnceLock<Option<TcpStream>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct Helper {
    pub mode: &'static str,
    pub marker: Option<PathBuf>,
    pub marker2: Option<PathBuf>,
    pub link: Option<String>,
    pub show: Option<&'static str>,
    pub count: Option<u64>,
    pub detach: bool,
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

    pub fn link(mut self, links: &super::Links) -> Helper {
        self.link = Some(links.addr().to_string());
        self
    }

    pub fn show(mut self, var: &'static str) -> Helper {
        self.show = Some(var);
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

    fn vars(&self) -> Vec<(&'static str, String)> {
        let mut vars = vec![(MODE, self.mode.to_string()), (ROLE, "child".to_string())];
        let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
        for (name, value) in [
            (MARKER, path(&self.marker)),
            (MARKER2, path(&self.marker2)),
            (LINK, self.link.clone()),
            (SHOW, self.show.map(str::to_string)),
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
    match mode.as_str() {
        "ignore" => {
            announce(&role);
            ignore_loop(&role);
        }
        "obey" => {
            announce(&role);
            await_soft(LIFETIME);
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
        "wait-gate" => {
            announce(&role);
            wait_gate();
            let code = std::env::var(COUNT).ok().and_then(|c| c.parse().ok());
            std::process::exit(code.unwrap_or(0));
        }
        "pong" => {
            announce(&role);
            pong();
        }
        "print-start" => {
            let start = lr_exec::process_start(std::process::id());
            say(&format!("start={start:?} start-end\n"));
            wait_gate();
            std::process::exit(0);
        }
        "no-console-cancel" => super::no_console::run(false),
        "no-console-hosted" => super::no_console::run(true),
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
            wait_gate();
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

fn linked() -> Option<&'static TcpStream> {
    LINKED
        .get_or_init(|| {
            let addr = std::env::var(LINK).ok()?;
            let role = std::env::var(ROLE).unwrap_or_else(|_| "child".to_string());
            let shown = std::env::var(SHOW)
                .map(|var| format!("{var}=[{}]", std::env::var(&var).unwrap_or_default()))
                .unwrap_or_default();
            let mut stream = TcpStream::connect(addr).unwrap();
            writeln!(stream, "{role} {} {shown}", std::process::id()).unwrap();
            Some(stream)
        })
        .as_ref()
}

fn wait_gate() {
    let mut stream = linked().expect("a gate needs a link");
    let _ = stream.read(&mut [0u8; 1]);
}

fn pong() -> ! {
    let mut stream = linked().expect("pong needs a link");
    let mut byte = [0u8; 1];
    while matches!(stream.read(&mut byte), Ok(1)) {
        if stream.write_all(&byte).is_err() {
            break;
        }
    }
    std::process::exit(0);
}

pub fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn announce(role: &str) {
    let pid = std::process::id();
    linked();
    if let Ok(addr) = std::env::var(READY) {
        let _ = TcpStream::connect(addr).and_then(|mut s| s.write_all(b"r"));
    }
    say(&format!("{role}-pid={pid} {role}-pid-end\n"));
}

fn ignore_loop(role: &str) -> ! {
    let marker = std::env::var(if role == "grandchild" {
        MARKER2
    } else {
        MARKER
    })
    .ok();
    let until = Instant::now() + LIFETIME;
    let mut written = false;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if await_soft(left) && !written {
            written = true;
            if let Some(path) = &marker {
                std::fs::write(path, b"soft").unwrap();
            }
            say(&format!("got-soft-{role}\n"));
        }
    }
    std::process::exit(0);
}

fn spawn_grandchild() {
    let ready = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST_NAME, "--nocapture", "--quiet"]);
    cmd.env(MODE, "ignore")
        .env(ROLE, "grandchild")
        .env(READY, ready.local_addr().unwrap().to_string());
    let detach = std::env::var(DETACH).map(|v| v == "1").unwrap_or(true);
    if detach {
        detach_command(&mut cmd);
    }
    let mut child = cmd.spawn().unwrap();
    let (tell, told) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ready = ready.accept().and_then(|(mut stream, _)| {
            stream.set_read_timeout(Some(READY_LIMIT))?;
            stream.read_exact(&mut [0u8; 1])
        });
        let _ = tell.send(ready.is_ok());
    });
    if told.recv_timeout(READY_LIMIT) != Ok(true) {
        let _ = child.kill();
        let _ = child.wait();
        say("grandchild-not-ready\n");
        std::process::exit(3);
    }
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
static SOFT_SEEN: std::sync::Mutex<u32> = std::sync::Mutex::new(0);
#[cfg(windows)]
static SOFT_WAKE: std::sync::Condvar = std::sync::Condvar::new();

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
            let mut seen = SOFT_SEEN.lock().unwrap_or_else(|e| e.into_inner());
            *seen += 1;
            SOFT_WAKE.notify_all();
            1
        } else {
            0
        }
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

#[cfg(windows)]
fn await_soft(limit: Duration) -> bool {
    let seen = SOFT_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let (mut seen, _) = SOFT_WAKE
        .wait_timeout_while(seen, limit, |n| *n == 0)
        .unwrap_or_else(|e| e.into_inner());
    if *seen > 0 {
        *seen -= 1;
        true
    } else {
        false
    }
}

#[cfg(unix)]
static SOFT_PIPE: [std::sync::atomic::AtomicI32; 2] = [
    std::sync::atomic::AtomicI32::new(-1),
    std::sync::atomic::AtomicI32::new(-1),
];

#[cfg(unix)]
fn install_soft_handler() {
    use std::sync::atomic::Ordering;
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
        let wake = SOFT_PIPE[1].load(Ordering::SeqCst);
        if wake >= 0 {
            unsafe {
                libc::write(wake, b"s".as_ptr() as *const libc::c_void, 1);
            }
        }
    }
    let mut fds: [libc::c_int; 2] = [-1; 2];
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) == 0 {
            for fd in fds {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            let flags = libc::fcntl(fds[1], libc::F_GETFL);
            libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
            SOFT_PIPE[0].store(fds[0], Ordering::SeqCst);
            SOFT_PIPE[1].store(fds[1], Ordering::SeqCst);
        }
        libc::signal(
            libc::SIGTERM,
            handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

#[cfg(unix)]
fn await_soft(limit: Duration) -> bool {
    use std::sync::atomic::Ordering;
    let fd = SOFT_PIPE[0].load(Ordering::SeqCst);
    let until = Instant::now() + limit;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        if unsafe { libc::poll(&mut poll, 1, ms) } > 0 {
            let mut byte = 0u8;
            unsafe {
                libc::read(fd, &mut byte as *mut u8 as *mut libc::c_void, 1);
            }
            return true;
        }
    }
    false
}
