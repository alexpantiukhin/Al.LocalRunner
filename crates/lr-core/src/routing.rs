use crate::matching::{satisfies, Mismatch};
use crate::model::Zone;
use crate::queue::{QueueEntry, RunnerView};
use crate::scheduler::Reject;
use crate::zone::Site;
use std::cmp::Reverse;

/// Checks that `runner` suits `entry` (spec 3.7, 4) and returns the pairwise zone of the
/// runner relative to the client.
///
/// The zone is computed from the two connections, not taken from the runner report.
pub fn suitable(entry: &QueueEntry, runner: &RunnerView, site: &Site) -> Result<Zone, Mismatch> {
    let zone = site.zone_between(
        &entry.client.endpoint,
        &entry.client.label,
        &runner.endpoint,
        &runner.name,
    );
    let mut caps = runner.caps.clone();
    caps.zone = zone;
    let req = &entry.order.requirements;
    let reasons = match satisfies(&caps, req) {
        Ok(()) => Vec::new(),
        Err(m) => m.reasons,
    };
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
/// An order limited to the `local` zone is refused when no runner of the client's installation
/// is online, and with numbers when they are online but none suits. Any other order is refused
/// with numbers when no registered runner suits it, online or not; when every suitable runner
/// is offline it is accepted and waits.
pub fn check_runners(
    entry: &QueueEntry,
    runners: &[RunnerView],
    site: &Site,
) -> Result<(), Reject> {
    let candidates: Vec<&RunnerView> = if entry.order.requirements.local_only() {
        let own: Vec<&RunnerView> = runners
            .iter()
            .filter(|r| r.online)
            .filter(|r| {
                site.zone_between(
                    &entry.client.endpoint,
                    &entry.client.label,
                    &r.endpoint,
                    &r.name,
                ) == Zone::Local
            })
            .collect();
        if own.is_empty() {
            return Err(Reject::NoRunnerOnMachine);
        }
        own
    } else {
        runners.iter().collect()
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
