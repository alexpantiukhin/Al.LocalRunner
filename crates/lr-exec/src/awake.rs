//! [`SystemKeepAwake`], the real [`KeepAwake`] (spec 10, "Запрет сна"): on Windows one
//! dedicated thread holds `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`, on
//! macOS `/usr/bin/caffeinate -i -w <runner pid>` runs while held, elsewhere nothing is done
//! (Linux: "по умолчанию нет").

use lr_abstractions::KeepAwake;
use std::sync::Mutex;

/// What actually keeps one OS awake; `start` is called only when not running, `stop` only
/// when running.
pub trait Inhibitor: Send {
    /// Starts keeping the machine awake.
    fn start(&mut self);
    /// Lets it sleep again.
    fn stop(&mut self);
}

/// [`KeepAwake`] over an [`Inhibitor`]: repeated holds are one hold, a release without a hold
/// does nothing.
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
    fn hold(&self) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !s.1 {
            s.0.start();
            s.1 = true;
        }
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

    #[derive(Default)]
    pub struct Os {
        thread: Option<Sender<bool>>,
    }

    impl Os {
        fn send(&mut self, on: bool) {
            if self.thread.is_none() {
                let (tx, rx) = channel::<bool>();
                let spawned =
                    std::thread::Builder::new()
                        .name("keep-awake".into())
                        .spawn(move || {
                            while let Ok(on) = rx.recv() {
                                let flags = if on { HOLD_FLAGS } else { ES_CONTINUOUS };
                                unsafe { SetThreadExecutionState(flags) };
                            }
                            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                        });
                if spawned.is_ok() {
                    self.thread = Some(tx);
                }
            }
            if let Some(tx) = &self.thread {
                let _ = tx.send(on);
            }
        }
    }

    impl Inhibitor for Os {
        fn start(&mut self) {
            self.send(true);
        }

        fn stop(&mut self) {
            self.send(false);
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::{caffeinate_command, Inhibitor};
    use std::process::{Child, Command, Stdio};

    #[derive(Default)]
    pub struct Os {
        child: Option<Child>,
    }

    impl Inhibitor for Os {
        fn start(&mut self) {
            let (program, args) = caffeinate_command(std::process::id());
            self.child = Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok();
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

    #[derive(Default)]
    pub struct Os;

    impl Inhibitor for Os {
        fn start(&mut self) {}

        fn stop(&mut self) {}
    }
}

/// The keep-awake of this OS.
pub type SystemKeepAwake = Holder<os::Os>;

/// A [`SystemKeepAwake`] that is not held.
pub fn system_keep_awake() -> SystemKeepAwake {
    Holder::new(os::Os::default())
}
