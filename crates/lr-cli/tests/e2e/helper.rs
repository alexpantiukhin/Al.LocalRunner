use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MODE: &str = "LR_E2E_MODE";
const ROLE: &str = "LR_E2E_ROLE";
const PIDS: &str = "LR_E2E_PIDS";
const GATE: &str = "LR_E2E_GATE";
const TEST_NAME: &str = "helper_process";
const LONGEST: Duration = Duration::from_secs(120);

pub enum Mode {
    Tree,
    Wait,
}

pub fn helper_command(mode: Mode, pids: &Path, gate: &Path) -> String {
    let exe = std::env::current_exe().unwrap().display().to_string();
    let args = format!("--exact {TEST_NAME} --nocapture --quiet --test-threads=1");
    let mode = match mode {
        Mode::Tree => "tree",
        Mode::Wait => "wait",
    };
    let vars = [
        (MODE, mode.to_string()),
        (ROLE, "child".to_string()),
        (PIDS, pids.display().to_string()),
        (GATE, gate.display().to_string()),
    ];
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

fn pid_file(pids: &Path, role: &str) -> PathBuf {
    let mut name = pids.file_name().unwrap().to_os_string();
    name.push(format!(".{role}"));
    pids.with_file_name(name)
}

pub fn recorded_pid(pids: &Path, role: &str) -> Option<u32> {
    std::fs::read_to_string(pid_file(pids, role))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn announce(pids: &Path, role: &str) {
    let path = pid_file(pids, role);
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    std::fs::write(&tmp, std::process::id().to_string()).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    println!("{role}-started");
}

fn wait_gate(gate: &Path) {
    let started = Instant::now();
    while !gate.exists() && started.elapsed() < LONGEST {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_grandchild(pids: &Path, gate: &Path) {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST_NAME, "--nocapture", "--quiet"])
        .env(MODE, "wait")
        .env(ROLE, "grandchild")
        .env(PIDS, pids)
        .env(GATE, gate)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut cmd);
    std::mem::forget(cmd.spawn().unwrap());
    let started = Instant::now();
    while recorded_pid(pids, "grandchild").is_none() && started.elapsed() < LONGEST {
        std::thread::sleep(Duration::from_millis(10));
    }
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
    let role = std::env::var(ROLE).unwrap();
    let pids = PathBuf::from(std::env::var(PIDS).unwrap());
    let gate = PathBuf::from(std::env::var(GATE).unwrap());
    if mode == "tree" {
        spawn_grandchild(&pids, &gate);
    }
    announce(&pids, &role);
    wait_gate(&gate);
    println!("{role}-done");
    std::process::exit(0);
}
