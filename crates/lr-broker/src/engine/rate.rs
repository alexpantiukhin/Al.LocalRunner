use super::Engine;
use lr_core::Timestamp;
use std::collections::{HashMap, VecDeque};

const WINDOW_MS: u64 = 60_000;

#[derive(Default)]
pub(crate) struct Rate {
    served: HashMap<String, VecDeque<Timestamp>>,
}

impl Rate {
    fn admit(&mut self, client: &str, now: Timestamp, limit: usize) -> bool {
        let log = self.served.entry(client.to_string()).or_default();
        while log
            .front()
            .is_some_and(|t| now.millis_since(*t) >= WINDOW_MS)
        {
            log.pop_front();
        }
        if log.len() >= limit {
            return false;
        }
        log.push_back(now);
        true
    }

    fn forget_idle(&mut self, now: Timestamp) {
        self.served
            .retain(|_, log| log.back().is_some_and(|t| now.millis_since(*t) < WINDOW_MS));
    }
}

impl Engine {
    pub(crate) fn rate_limited(&mut self, client: &str) -> Option<String> {
        let limit = self.policy.file().limits.api_requests_per_min.get();
        let now = self.now();
        if self.rate.admit(client, now, limit as usize) {
            return None;
        }
        Some(format!(
            "больше {limit} запросов API за минуту у клиента; повторите позже"
        ))
    }

    pub(crate) fn forget_idle_rates(&mut self) {
        let now = self.now();
        self.rate.forget_idle(now);
    }
}
