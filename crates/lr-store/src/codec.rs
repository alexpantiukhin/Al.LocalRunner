//! Conversions between the Rust rows of `lr-abstractions` and SQLite values: enums as lowercase
//! text, times and counters as `INTEGER` (u64 bit-cast to i64, lossless), sets and maps as JSON.

use lr_abstractions::{CodeSource, MachineKind, OutboxKind, RunnerState, ServiceState, SourceKind};
use lr_core::{JobState, StepState, Timestamp, Zone};
use rusqlite::types::Type;
use rusqlite::Row;
use std::collections::BTreeSet;

pub(crate) trait Text: Sized {
    fn text(&self) -> &'static str;
    fn parse(s: &str) -> Option<Self>;
}

macro_rules! text_enum {
    ($t:ty { $($v:ident => $s:literal),* $(,)? }) => {
        impl Text for $t {
            fn text(&self) -> &'static str {
                match self { $(<$t>::$v => $s),* }
            }
            fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some(<$t>::$v),)* _ => None }
            }
        }
    };
}

text_enum!(JobState {
    Queued => "queued", Running => "running", Cancelling => "cancelling", Passed => "passed",
    Failed => "failed", Timeout => "timeout", Cancelled => "cancelled", Lost => "lost",
});
text_enum!(StepState {
    Queued => "queued", Running => "running", Passed => "passed", Failed => "failed",
    Timeout => "timeout", Skipped => "skipped", Cancelled => "cancelled",
});
text_enum!(Zone { Local => "local", Lan => "lan", Remote => "remote" });
text_enum!(SourceKind { Session => "session", Github => "github" });
text_enum!(CodeSource { WorkTree => "work_tree", Commit => "commit" });
text_enum!(MachineKind { Hub => "hub", Runner => "runner" });
text_enum!(RunnerState {
    Registered => "registered", Online => "online", Offline => "offline",
    Draining => "draining", Removed => "removed",
});
text_enum!(ServiceState {
    Starting => "starting", Idle => "idle", Busy => "busy", Stopping => "stopping",
    Stopped => "stopped", Failed => "failed", Unavailable => "unavailable",
});
text_enum!(OutboxKind {
    Order => "order", Cancel => "cancel", Started => "started", Chunk => "chunk",
    StepResult => "step_result", JobResult => "job_result", CancelAck => "cancel_ack",
});

pub(crate) fn conversion(idx: usize, ty: Type, what: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, ty, what.into())
}

pub(crate) fn int(v: u64) -> i64 {
    v as i64
}

pub(crate) fn time(t: Timestamp) -> i64 {
    t.0 as i64
}

pub(crate) fn opt_time(t: Option<Timestamp>) -> Option<i64> {
    t.map(time)
}

pub(crate) fn opt_int(v: Option<u64>) -> Option<i64> {
    v.map(int)
}

pub(crate) fn col_enum<T: Text>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T> {
    let s: String = row.get(idx)?;
    T::parse(&s).ok_or_else(|| conversion(idx, Type::Text, format!("unknown value {s:?}")))
}

pub(crate) fn col_opt_enum<T: Text>(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<T>> {
    let s: Option<String> = row.get(idx)?;
    s.map(|s| {
        T::parse(&s).ok_or_else(|| conversion(idx, Type::Text, format!("unknown value {s:?}")))
    })
    .transpose()
}

pub(crate) fn col_u64(row: &Row<'_>, idx: usize) -> rusqlite::Result<u64> {
    Ok(row.get::<_, i64>(idx)? as u64)
}

pub(crate) fn col_opt_u64(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<u64>> {
    Ok(row.get::<_, Option<i64>>(idx)?.map(|v| v as u64))
}

pub(crate) fn col_time(row: &Row<'_>, idx: usize) -> rusqlite::Result<Timestamp> {
    col_u64(row, idx).map(Timestamp)
}

pub(crate) fn col_opt_time(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<Timestamp>> {
    col_opt_u64(row, idx).map(|v| v.map(Timestamp))
}

pub(crate) fn col_u32(row: &Row<'_>, idx: usize) -> rusqlite::Result<u32> {
    let v: i64 = row.get(idx)?;
    u32::try_from(v).map_err(|_| conversion(idx, Type::Integer, format!("{v} is not a u32")))
}

pub(crate) fn col_json<T: serde::de::DeserializeOwned>(
    row: &Row<'_>,
    idx: usize,
) -> rusqlite::Result<T> {
    let s: String = row.get(idx)?;
    serde_json::from_str(&s).map_err(|e| conversion(idx, Type::Text, format!("bad JSON: {e}")))
}

pub(crate) fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

pub(crate) type StringSet = BTreeSet<String>;
