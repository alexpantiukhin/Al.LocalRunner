//! [`SystemKeepAwake`], the real [`KeepAwake`] (spec 10, "Запрет сна"): on Windows one
//! dedicated thread holds `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`, on
//! macOS `/usr/bin/caffeinate -i -w <runner pid>` runs while held, elsewhere nothing is done
//! (Linux: "по умолчанию нет").

use lr_abstractions::KeepAwake;
use std::sync::Mutex;

/// What actually keeps one OS awake; `start` is called only when not running, `stop` only
/// when running.
pub trait Inhibitor: Send {
    /// Starts keeping the machine awake; the error says why it could not.
    fn start(&mut self) -> Result<(), String>;
    /// Lets it sleep again.
    fn stop(&mut self);
}

/// [`KeepAwake`] over an [`Inhibitor`]: repeated holds are one hold, a release without a hold
/// does nothing, a failed start leaves it not held.
pub struct Holder<I: Inhibitor> {
    state: Mutex<(I, bool)>,
}

impl<I: Inhibitor> Holder<I> {
    /// Not held.
    pub fn new(inhibitor: I) -> Holder<I> {
        Holder {
            state: Mutex::new((inhibitor, false)),
        }
    }
}

impl<I: Inhibitor> KeepAwake for Holder<I> {
    fn hold(&self) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !s.1 {
            s.0.start()?;
            s.1 = true;
        }
        Ok(())
    }

    fn release(&self) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.1 {
            s.0.stop();
            s.1 = false;
        }
    }
}

/// The program and arguments that keep macOS awake until released or until the process
/// `pid` (the runner) ends.
pub fn caffeinate_command(pid: u32) -> (&'static str, Vec<String>) {
    (
        "/usr/bin/caffeinate",
        vec!["-i".into(), "-w".into(), pid.to_string()],
    )
}

/// `SetThreadExecutionState` flags while held.
#[cfg(windows)]
pub const HOLD_FLAGS: u32 = windows_sys::Win32::System::Power::ES_CONTINUOUS
    | windows_sys::Win32::System::Power::ES_SYSTEM_REQUIRED;

#[cfg(windows)]
mod os {
    use super::{Inhibitor, HOLD_FLAGS};
    use std::sync::mpsc::{channel, Sender};
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};

    pub struct Os {
        thread: Option<Sender<bool>>,
    }

    pub fn new() -> Os {
        Os { thread: None }
    }

    impl Os {
        fn send(&mut self, on: bool) -> Result<(), String> {
            if self.thread.is_none() {
                let (tx, rx) = channel::<bool>();
                std::thread::Builder::new()
                    .name("keep-awake".into())
                    .spawn(move || {
                        while let Ok(on) = rx.recv() {
                            let flags = if on { HOLD_FLAGS } else { ES_CONTINUOUS };
                            unsafe { SetThreadExecutionState(flags) };
                        }
                        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                    })
                    .map_err(|e| format!("keep-awake thread not started: {e}"))?;
                self.thread = Some(tx);
            }
            let sent = self.thread.as_ref().is_some_and(|tx| tx.send(on).is_ok());
            if !sent {
                self.thread = None;
                return Err("keep-awake thread is gone".into());
            }
            Ok(())
        }
    }

    impl Inhibitor for Os {
        fn start(&mut self) -> Result<(), String> {
            self.send(true)
        }

        fn stop(&mut self) {
            let _ = self.send(false);
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::{caffeinate_command, Inhibitor};
    use std::process::{Child, Command, Stdio};

    pub struct Os {
        child: Option<Child>,
    }

    pub fn new() -> Os {
        Os { child: None }
    }

    impl Inhibitor for Os {
        fn start(&mut self) -> Result<(), String> {
            let (program, args) = caffeinate_command(std::process::id());
            let child = Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("{program} not started: {e}"))?;
            self.child = Some(child);
            Ok(())
        }

        fn stop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    impl Drop for Os {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod os {
    use super::Inhibitor;

    pub struct Os;

    pub fn new() -> Os {
        Os
    }

    impl Inhibitor for Os {
        fn start(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn stop(&mut self) {}
    }
}

/// The keep-awake of this OS.
pub type SystemKeepAwake = Holder<os::Os>;

/// A [`SystemKeepAwake`] that is not held.
pub fn system_keep_awake() -> SystemKeepAwake {
    Holder::new(os::new())
}
