use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub(crate) struct Reaped(AtomicBool);

impl Reaped {
    pub(crate) fn mark(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_marked(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub(crate) fn run_if_alive(&self, action: impl FnOnce()) {
        if !self.is_marked() {
            action();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Reaped;

    #[test]
    fn action_runs_until_marked_then_never() {
        let reaped = Reaped::default();
        let mut calls = 0;
        reaped.run_if_alive(|| calls += 1);
        reaped.mark();
        reaped.run_if_alive(|| calls += 1);
        assert_eq!(calls, 1);
        assert!(reaped.is_marked());
    }
}
