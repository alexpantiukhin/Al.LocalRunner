use crate::clock::{Clock, Sleep};
use lr_core::Timestamp;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

#[derive(Default)]
struct State {
    now: u64,
    next_id: u64,
    sleepers: Vec<(u64, u64, Waker)>,
}

/// A clock that moves only on [`ManualClock::advance`]; advancing wakes every sleep whose
/// deadline has passed. Clones share the same time.
#[derive(Clone)]
pub struct ManualClock {
    state: Arc<Mutex<State>>,
}

impl ManualClock {
    /// A clock standing at `start`.
    pub fn new(start: Timestamp) -> ManualClock {
        ManualClock {
            state: Arc::new(Mutex::new(State {
                now: start.0,
                ..State::default()
            })),
        }
    }

    /// Moves time forward by `by` and wakes the sleeps that are due.
    pub fn advance(&self, by: Duration) {
        let due: Vec<Waker> = {
            let mut s = self.state.lock().unwrap();
            s.now = s.now.saturating_add(by.as_millis() as u64);
            let now = s.now;
            let (due, rest) = std::mem::take(&mut s.sleepers)
                .into_iter()
                .partition(|(_, deadline, _)| *deadline <= now);
            s.sleepers = rest;
            due.into_iter().map(|(_, _, w)| w).collect()
        };
        for waker in due {
            waker.wake();
        }
    }

    /// Number of sleeps currently waiting for a future time.
    pub fn sleepers(&self) -> usize {
        self.state.lock().unwrap().sleepers.len()
    }

    /// Yields to other tasks until at least `count` sleeps wait on this clock, so a test can
    /// advance time only once the code under test is actually waiting. Panics after
    /// [`ManualClock::DEFAULT_YIELDS`] yields instead of hanging.
    pub async fn wait_for_sleepers(&self, count: usize) {
        if let Err(seen) = self
            .wait_for_sleepers_within(count, Self::DEFAULT_YIELDS)
            .await
        {
            panic!(
                "waited for {count} sleepers, only {seen} after {} yields",
                Self::DEFAULT_YIELDS
            );
        }
    }

    /// Yields budget of [`ManualClock::wait_for_sleepers`].
    pub const DEFAULT_YIELDS: usize = 10_000;

    /// Like [`ManualClock::wait_for_sleepers`] but gives up after `max_yields` yields and
    /// returns the number of sleepers seen then.
    pub async fn wait_for_sleepers_within(
        &self,
        count: usize,
        max_yields: usize,
    ) -> Result<(), usize> {
        for _ in 0..max_yields {
            if self.sleepers() >= count {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
        match self.sleepers() {
            n if n >= count => Ok(()),
            n => Err(n),
        }
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.state.lock().unwrap().now)
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        Box::pin(ManualSleep {
            state: self.state.clone(),
            id: s.next_id,
            deadline: s.now.saturating_add(duration.as_millis() as u64),
        })
    }
}

struct ManualSleep {
    state: Arc<Mutex<State>>,
    id: u64,
    deadline: u64,
}

impl Future for ManualSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut s = self.state.lock().unwrap();
        if s.now >= self.deadline {
            s.sleepers.retain(|(id, _, _)| *id != self.id);
            return Poll::Ready(());
        }
        match s.sleepers.iter_mut().find(|(id, _, _)| *id == self.id) {
            Some(entry) => entry.2 = cx.waker().clone(),
            None => s
                .sleepers
                .push((self.id, self.deadline, cx.waker().clone())),
        }
        Poll::Pending
    }
}

impl Drop for ManualSleep {
    fn drop(&mut self) {
        if let Ok(mut s) = self.state.lock() {
            s.sleepers.retain(|(id, _, _)| *id != self.id);
        }
    }
}
