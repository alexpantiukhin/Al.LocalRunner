use crate::db::{Fail, R};
use lr_abstractions::StoreError;

/// The seq rule of spec 3.9: `true` for the next seq, `false` for a repeat, a gap is refused.
pub(crate) fn decide(last: u64, seq: u64) -> R<bool> {
    if seq <= last {
        Ok(false)
    } else if seq == last + 1 {
        Ok(true)
    } else {
        Err(Fail::Store(StoreError::SeqGap {
            expected: last + 1,
            got: seq,
        }))
    }
}
