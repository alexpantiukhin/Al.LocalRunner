use lr_abstractions::{Clock, Level, LogSink, Record, Role};
use std::sync::Arc;

pub(crate) struct Diag {
    pub(crate) log: Arc<dyn LogSink>,
    pub(crate) clock: Arc<dyn Clock>,
}

impl Diag {
    pub(crate) fn event(&self, level: Level, message: &str, fields: &[(&str, &str)]) {
        let mut r = Record::new(self.clock.now(), level, Role::Runner, "agent", message);
        for (k, v) in fields {
            r = r.field(k, v);
        }
        self.log.write(&r);
    }
}
