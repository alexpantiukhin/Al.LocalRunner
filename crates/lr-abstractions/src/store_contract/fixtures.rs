//! Rows used by the contract cases.

use crate::store::{
    CodeSource, Intake, JobKey, JobRow, JobSpecRow, Join, JoinCodeRow, MachineKind, MachineRow,
    OrderRow, RunnerRow, RunnerState, SourceKind, StepRow,
};
use lr_core::{JobState, StepState, Timestamp};
use std::collections::{BTreeMap, BTreeSet};

/// Base time of every case.
pub const T0: Timestamp = Timestamp(1_700_000_000_000);

/// `T0` plus `secs` seconds.
pub fn at(secs: u64) -> Timestamp {
    T0.plus_secs(secs)
}

/// An order of `client` for `job` of profile `profile`, queued at `queued_at`, with steps
/// `build` and `test`.
pub fn intake(client: &str, job: &str, profile: &str, queued_at: Timestamp) -> Intake {
    let key = JobKey::new(client, job);
    let step = |ord: u32, name: &str| StepRow {
        ord,
        name: name.to_string(),
        command: format!("run {name}"),
        state: StepState::Queued,
        exit_code: None,
        started_at: None,
        finished_at: None,
        timeout_s: 600,
    };
    Intake {
        job: JobRow {
            key: key.clone(),
            project: "owner/repo".into(),
            source: SourceKind::Session,
            label: "agent".into(),
            note: None,
            branch: Some("main".into()),
            cwd: "/work/repo".into(),
            profile: profile.to_string(),
            platform: Some("linux".into()),
            runner: None,
            commit_sha: Some("abc123".into()),
            client_machine: format!("m-{client}"),
            client_label: format!("label-{client}"),
            os_user: "alex".into(),
            secrets: vec!["registry_auth".into()],
            code_source: CodeSource::Commit,
            queued_at,
            started_at: None,
            finished_at: None,
            state: JobState::Queued,
            exit_code: None,
            cancelled_by: None,
            cancel_reason: None,
            peak_mem_mb: None,
            peak_disk_mb: None,
        },
        steps: vec![step(0, "build"), step(1, "test")],
        spec: JobSpecRow {
            resolved_json: format!("{{\"job\":\"{job}\"}}"),
            profile_commit: None,
        },
        github: None,
        order: OrderRow {
            key,
            client_machine: format!("m-{client}"),
            spec_json: format!("{{\"job\":\"{job}\"}}"),
            platform: Some("linux".into()),
            same_machine: false,
            priority_hint: None,
            enqueued_at: queued_at,
            state: "queued".into(),
        },
    }
}

/// A key record named `name` holding `key`.
pub fn machine(name: &str, kind: MachineKind, key: &str) -> MachineRow {
    MachineRow {
        name: name.to_string(),
        machine_id: format!("id-{name}"),
        kind,
        label: "agent".into(),
        public_key: key.to_string(),
        registered_at: T0,
        last_seen: None,
        revoked_at: None,
    }
}

/// A code with `uses` joins for `kind`, valid for one hour from `T0`.
pub fn code(hash: &str, kind: MachineKind, uses: u32) -> JoinCodeRow {
    JoinCodeRow {
        code_hash: hash.to_string(),
        kind,
        name: None,
        label: Some("agent".into()),
        uses_left: uses,
        issued_at: T0,
        expires_at: at(3600),
        redeemed_at: None,
        machine_name: None,
    }
}

/// A join of `machine` with code `hash` at `at`.
pub fn join(hash: &str, machine: MachineRow, when: Timestamp) -> Join {
    Join {
        code_hash: hash.to_string(),
        at: when,
        machine,
    }
}

/// A registered runner `name` on machine `machine_name`.
pub fn runner(name: &str, machine_name: &str) -> RunnerRow {
    RunnerRow {
        name: name.to_string(),
        machine_name: machine_name.to_string(),
        programs: BTreeSet::from(["git".to_string()]),
        platforms: BTreeSet::from(["linux".to_string()]),
        tags: BTreeSet::new(),
        max_timeout_s: Some(3600),
        cache_max_mb: 40_000,
        cache_used_mb: 0,
        free_disk_mb: 100_000,
        services: BTreeMap::new(),
        accept_min_mem_mb: None,
        enforcement: None,
        version: Some("0.2.0".to_string()),
        last_seen: None,
        state: RunnerState::Registered,
    }
}
