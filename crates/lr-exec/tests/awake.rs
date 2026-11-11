use lr_abstractions::KeepAwake;
use lr_exec::{caffeinate_command, Holder, Inhibitor};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Recording(Arc<Mutex<Vec<&'static str>>>);

impl Inhibitor for Recording {
    fn start(&mut self) {
        self.0.lock().unwrap().push("start");
    }

    fn stop(&mut self) {
        self.0.lock().unwrap().push("stop");
    }
}

#[test]
fn repeated_holds_are_one_hold_and_a_release_without_a_hold_does_nothing() {
    let calls = Recording::default();
    let holder = Holder::new(calls.clone());
    holder.release();
    holder.hold();
    holder.hold();
    holder.release();
    holder.release();
    holder.hold();
    assert_eq!(*calls.0.lock().unwrap(), vec!["start", "stop", "start"]);
}

#[test]
fn macos_keeps_awake_with_caffeinate_by_absolute_path_tied_to_the_runner() {
    let (program, args) = caffeinate_command(4242);
    assert_eq!(program, "/usr/bin/caffeinate");
    assert_eq!(args, vec!["-i", "-w", "4242"]);
}

#[cfg(windows)]
#[test]
fn windows_holds_system_required_continuously() {
    assert_eq!(lr_exec::HOLD_FLAGS, 0x8000_0000 | 0x0000_0001);
}
