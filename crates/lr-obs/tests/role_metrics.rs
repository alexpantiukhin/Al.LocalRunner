//! The names and label sets the roles send (`lr_abstractions::metric`) against the catalog.

use lr_abstractions::Meter;
use lr_core::Timestamp;
use lr_obs::metrics::{Metrics, CATALOG, CLIENT_LABEL, PREFIX};

fn metrics() -> Metrics {
    Metrics::new(Timestamp(1_700_000_000_000), false)
}

#[test]
fn every_name_the_roles_use_is_in_the_catalog() {
    use lr_abstractions::metric::ALL;
    for name in ALL {
        assert!(CATALOG.iter().any(|d| d.name == *name), "{name}");
    }
    let service = [
        "service_up",
        "service_starts_total",
        "service_failures_total",
    ];
    for def in CATALOG {
        let short = def.name.trim_start_matches(PREFIX);
        let by_roles = ALL.contains(&def.name) || short == "log_dropped_total";
        assert!(
            by_roles || service.contains(&short),
            "{} has no writer",
            def.name
        );
    }
}

#[test]
fn updates_as_the_roles_send_them_are_all_accepted() {
    use lr_abstractions::metric::*;
    let m = metrics();
    let load = [(POOL, POOL_VALUE), (RUNNER, "r1")];
    m.set(QUEUE_LENGTH, &[(PLATFORM, SAME_MACHINE)], 1.0);
    m.set(QUEUE_LENGTH, &[(PLATFORM, OTHER_PLATFORM)], 0.0);
    m.observe(
        QUEUE_WAIT_SECONDS,
        &[(PLATFORM, "linux"), (CLIENT, "ci")],
        3.0,
    );
    m.set(RUNNING_JOBS, &load, 1.0);
    m.set(POOL_MEMORY_USED_MB, &load, 512.0);
    m.set(POOL_CPU_USED, &load, 2.0);
    for reason in ["runner_busy", "memory", "cpu", "tag", "rule"] {
        m.set(SATURATION_WAIT_SECONDS, &[(REASON, reason)], 0.0);
    }
    for result in [
        "passed",
        "failed",
        "timeout",
        "cancelled",
        "lost",
        "rejected",
    ] {
        let labels = [(RESULT, result), (PLATFORM, "linux"), (CLIENT, "ci")];
        m.add(ORDERS_TOTAL, &labels, 1);
        m.observe(JOB_DURATION_SECONDS, &labels, 1.0);
    }
    m.set(RUNNER_ONLINE, &[(RUNNER, "r1")], 1.0);
    m.set(CACHE_USED_MB, &[(RUNNER, "r1")], 600.0);
    for reason in ["size", "age"] {
        m.add(CACHE_EVICTED_TOTAL, &[(RUNNER, "r1"), (REASON, reason)], 1);
    }
    for role in ["hub", "broker", "runner"] {
        m.set(OUTBOX_PENDING, &[(ROLE, role)], 0.0);
        m.set(OUTBOX_OLDEST_AGE_SECONDS, &[(ROLE, role)], 0.0);
    }
    for result in ["ok", "error"] {
        m.add(POLICY_RELOAD_TOTAL, &[(RESULT, result)], 1);
    }
    assert_eq!(m.rejected(), 0);
    assert!(m
        .snapshot()
        .iter()
        .all(|p| p.labels.iter().all(|(k, _)| k != CLIENT_LABEL)));
}
