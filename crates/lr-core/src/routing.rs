use crate::matching::{satisfies, Mismatch, MismatchReason};
use crate::model::Zone;
use crate::queue::{QueueEntry, RunnerView};
use crate::scheduler::Reject;
use crate::zone::Site;
use std::cmp::Reverse;

/// Checks that `runner` suits `entry` (spec 3.7, 4) and returns the pairwise zone of the
/// runner relative to the client.
///
/// The zone is computed from the two connections, not taken from the runner report. A
/// same-machine order additionally needs zone `Local`: both connected over local IPC by the
/// same OS user.
pub fn suitable(entry: &QueueEntry, runner: &RunnerView, site: &Site) -> Result<Zone, Mismatch> {
    let zone = site.zone_between(&entry.client.endpoint, &runner.endpoint);
    let mut caps = runner.caps.clone();
    caps.zone = zone;
    let req = &entry.order.requirements;
    let mut reasons = match satisfies(&caps, req) {
        Ok(()) => Vec::new(),
        Err(m) => m.reasons,
    };
    if req.same_machine.is_some()
        && zone != Zone::Local
        && !reasons.contains(&MismatchReason::NotSameMachine)
    {
        reasons.push(MismatchReason::NotSameMachine);
    }
    if reasons.is_empty() {
        Ok(zone)
    } else {
        Err(Mismatch { reasons })
    }
}

/// Sorts suitable runners by preference (spec 3.7): by the position of their zone in
/// `zone_order` (zones not listed go last), then by the largest capacity, then by name.
pub fn sort_by_preference(candidates: &mut [(&RunnerView, Zone)], zone_order: &[Zone]) {
    let rank = |z: Zone| {
        zone_order
            .iter()
            .position(|x| *x == z)
            .unwrap_or(zone_order.len())
    };
    candidates.sort_by(|(a, za), (b, zb)| {
        rank(*za)
            .cmp(&rank(*zb))
            .then_with(|| Reverse(a.caps.mem_mb).cmp(&Reverse(b.caps.mem_mb)))
            .then_with(|| Reverse(a.caps.cpu).cmp(&Reverse(b.caps.cpu)))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Intake check against the registered runners (spec 3.4, 3.7, 4).
///
/// A same-machine order is refused when no runner of the client's installation is online, and
/// with numbers when they are online but none suits. Any other order is refused with numbers
/// when no registered runner suits it, online or not; when every suitable runner is offline it
/// is accepted and waits.
pub fn check_runners(
    entry: &QueueEntry,
    runners: &[RunnerView],
    site: &Site,
) -> Result<(), Reject> {
    let req = &entry.order.requirements;
    let candidates: Vec<&RunnerView> = match &req.same_machine {
        Some(machine) => {
            let own: Vec<&RunnerView> = runners
                .iter()
                .filter(|r| r.online && r.caps.machine.as_ref() == Some(machine))
                .filter(|r| site.zone_between(&entry.client.endpoint, &r.endpoint) == Zone::Local)
                .collect();
            if own.is_empty() {
                return Err(Reject::NoRunnerOnMachine);
            }
            own
        }
        None => runners.iter().collect(),
    };
    let mut mismatches = Vec::new();
    for runner in candidates {
        match suitable(entry, runner, site) {
            Ok(_) => return Ok(()),
            Err(m) => mismatches.push((runner.name.clone(), m)),
        }
    }
    Err(Reject::NoMatchingRunner {
        runners: mismatches,
    })
}
