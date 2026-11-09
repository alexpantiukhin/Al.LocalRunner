use lr_core::{JobState, Zone};

#[test]
fn zones_are_ordered_nearest_first() {
    assert!(Zone::Local < Zone::Lan && Zone::Lan < Zone::Remote);
}

#[test]
fn job_final_state_is_never_overwritten() {
    use JobState::*;
    for from in [Passed, Failed, Timeout, Cancelled, Lost] {
        assert!(from.is_final());
        for to in [
            Queued, Running, Cancelling, Passed, Failed, Timeout, Cancelled, Lost,
        ] {
            assert!(!from.can_become(to), "{from:?} -> {to:?}");
        }
    }
}

#[test]
fn waiting_job_cancel_is_immediate_and_running_goes_through_cancelling() {
    use JobState::*;
    assert!(Queued.can_become(Cancelled));
    assert!(Queued.can_become(Running));
    assert!(!Queued.can_become(Passed));
    assert!(!Running.can_become(Cancelled));
    assert!(Running.can_become(Cancelling));
    assert!(Cancelling.can_become(Cancelled));
    assert!(!Cancelling.is_final());
}

#[test]
fn running_job_reaches_every_reported_final_state() {
    use JobState::*;
    for to in [Passed, Failed, Timeout, Lost] {
        assert!(Running.can_become(to));
    }
    assert!(!Running.can_become(Queued));
}
