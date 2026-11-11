use super::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::metric::{RUNNER, SERVICE};
use lr_abstractions::{RunnerStore, ServiceState};
use lr_proto::{BrokerToRunner, ServiceReport, WireServiceStatus};
use lr_runner::{Lifetime, Reuse, ServiceSpec};
use std::future::Future;
use std::path::Path;

pub const SEC: Duration = Duration::from_secs(1);
pub const DB: &[(&str, &str)] = &[(RUNNER, "runner-a"), (SERVICE, "db")];

pub fn db(lifetime: Lifetime) -> ServiceSpec {
    ServiceSpec {
        start: "db-start".into(),
        ready_check: "db-ready".into(),
        stop: Some("db-stop".into()),
        ready_timeout_s: 5,
        stop_timeout_s: 3,
        memory_mb: 512,
        lifetime,
        idle_timeout_s: Some(100),
        max_lifetime_s: 1000,
        restart: 1,
        reuse: Reuse::SameClient,
        secrets: Vec::new(),
    }
}

pub fn rig_with(spec: ServiceSpec) -> Rig {
    let mut rig = Rig::new();
    rig.settings.services.insert("db".into(), spec);
    rig.files.mkdir(Path::new("/w/tree"));
    rig
}

pub fn needs_db(client: &str, project: &str) -> lr_proto::AssignSpec {
    let mut s = worktree_spec("/w/tree", vec![step("b", "make", 60)], 60);
    s.client = client.into();
    s.project = project.into();
    s.requirements.services = vec!["db".into()];
    s
}

pub fn service_env() -> Vec<(String, String)> {
    [
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/runner"),
        ("TEMP", "/tmp/runner"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

pub fn ready_fails(rig: &Rig) {
    rig.process.script(
        JobId("service:db:ready".into()),
        JobScript::new(vec![StepScript::new(Duration::ZERO, 1)]),
    );
}

pub fn commands(rig: &Rig) -> Vec<String> {
    rig.process
        .runs()
        .iter()
        .map(|r| r.steps[0].spec.command.clone())
        .collect()
}

/// Runs `fut` while the clock moves a second at a time.
pub async fn drive<T>(rig: &Rig, fut: impl Future<Output = T>) -> T {
    tokio::pin!(fut);
    for _ in 0..5_000 {
        tokio::select! {
            biased;
            v = &mut fut => return v,
            _ = rig.advance(SEC) => {}
        }
    }
    panic!("never finished: {}", rig.logs_text());
}

/// Every message up to the job result, acknowledging the job messages.
pub async fn collect(b: &Broker) -> Vec<RunnerToBroker> {
    let mut out = Vec::new();
    loop {
        let m = b.recv().await.expect("connection closed");
        if let Some((job, seq)) = m.sequence() {
            b.send(BrokerToRunner::Ack(Ack {
                job: job.clone(),
                seq,
            }))
            .await;
        }
        let done = matches!(m, RunnerToBroker::JobResult(_));
        out.push(m);
        if done {
            return out;
        }
    }
}

pub async fn run(
    rig: &Rig,
    b: &Broker,
    job: &str,
    spec: lr_proto::AssignSpec,
) -> Vec<RunnerToBroker> {
    b.assign(job, 1, spec).await;
    drive(rig, collect(b)).await
}

pub fn reports(msgs: &[RunnerToBroker]) -> Vec<Vec<ServiceReport>> {
    msgs.iter()
        .filter_map(|m| match m {
            RunnerToBroker::ServicesReport(r) => Some(r.services.clone()),
            _ => None,
        })
        .collect()
}

pub fn status_of(msgs: &[RunnerToBroker]) -> Option<WireServiceStatus> {
    msgs.iter().rev().find_map(|m| match m {
        RunnerToBroker::CapabilitiesUpdate(u) => u.capabilities.services.get("db").copied(),
        _ => None,
    })
}

pub async fn registry(rig: &Rig) -> Vec<(String, ServiceState)> {
    rig.store
        .services()
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.name, r.state))
        .collect()
}
