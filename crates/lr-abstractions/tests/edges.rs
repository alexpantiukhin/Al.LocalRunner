mod common;

use common::{clock, T0};
use lr_abstractions::fakes::{
    FakeDataSource, FakeProcessRunner, MemoryLogSink, MemoryMeter, MemorySecrets, MeterUpdate,
};
use lr_abstractions::{
    CancelRequest, DataError, DataSource, Level, LogSink, Meter, ProcessError, ProcessRef,
    ProcessRunner, Record, Role, SecretError, SecretSource, SecretValue,
};
use lr_core::JobId;

#[tokio::test]
async fn secret_source_returns_value_by_name_or_not_found() {
    let secrets = MemorySecrets::new();
    secrets.insert("registry_auth", "tok");
    assert_eq!(secrets.get("registry_auth").await.unwrap().expose(), "tok");
    assert_eq!(
        secrets.get("signing_key").await.unwrap_err(),
        SecretError::NotFound("signing_key".into())
    );
}

#[tokio::test]
async fn locked_secret_store_reports_unavailable_not_empty_value() {
    let secrets = MemorySecrets::new();
    secrets.insert("signing_key", "k");
    secrets.lock("keychain locked");
    assert!(matches!(
        secrets.get("signing_key").await,
        Err(SecretError::Unavailable { .. })
    ));
}

#[test]
fn secret_value_never_shows_in_debug_output() {
    let value = SecretValue::new("hunter2");
    let shown = format!("{value:?}");
    assert!(!shown.contains("hunter2"));
    assert_eq!(value.expose(), "hunter2");
}

fn record(message: &str) -> Record {
    Record::new(T0, Level::Info, Role::Broker, "engine", message)
}

#[test]
fn log_sink_keeps_structured_fields() {
    let sink = MemoryLogSink::new();
    sink.write(
        &record("order accepted")
            .field("job_id", "j1")
            .field("rule", "ci-first"),
    );
    let records = sink.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].role, Role::Broker);
    assert_eq!(records[0].component, "engine");
    assert_eq!(
        records[0].fields,
        vec![
            ("job_id".to_string(), "j1".to_string()),
            ("rule".to_string(), "ci-first".to_string())
        ]
    );
}

#[test]
fn full_bounded_sink_drops_and_counts_instead_of_blocking() {
    let sink = MemoryLogSink::bounded(2);
    for n in 0..5 {
        sink.write(&record(&format!("m{n}")));
    }
    assert_eq!(sink.records().len(), 2);
    assert_eq!(sink.dropped(), 3);
}

#[test]
fn memory_meter_keeps_updates_in_order_with_labels() {
    let meter = MemoryMeter::new();
    meter.add("localrunner_orders_total", &[("result", "passed")], 2);
    meter.set("localrunner_queue_length", &[("platform", "p1")], 3.0);
    meter.observe("localrunner_queue_wait_seconds", &[], 1.5);
    assert_eq!(
        meter.updates(),
        vec![
            MeterUpdate::Add(
                "localrunner_orders_total".into(),
                vec![("result".into(), "passed".into())],
                2
            ),
            MeterUpdate::Set(
                "localrunner_queue_length".into(),
                vec![("platform".into(), "p1".into())],
                3.0
            ),
            MeterUpdate::Observe("localrunner_queue_wait_seconds".into(), vec![], 1.5),
        ]
    );
}

#[tokio::test]
async fn data_source_serves_snapshot_and_streams_events_to_each_subscriber() {
    let source: FakeDataSource<Vec<String>, u32> = FakeDataSource::new(vec!["q".into()]);
    let mut first = source.subscribe().await.unwrap();
    let mut second = source.subscribe().await.unwrap();
    source.publish(1);
    source.publish(2);
    assert_eq!(first.next().await, Some(1));
    assert_eq!(first.next().await, Some(2));
    assert_eq!(second.next().await, Some(1));
    source.set_snapshot(vec!["q".into(), "r".into()]);
    assert_eq!(source.snapshot().await.unwrap().len(), 2);
}

#[tokio::test]
async fn data_source_reads_logs_and_records_cancel_requests() {
    let source: FakeDataSource<(), ()> = FakeDataSource::new(());
    let id = JobId("j1".into());
    source.set_log(id.clone(), b"line\n".to_vec());
    assert_eq!(source.log(&id).await.unwrap(), b"line\n".to_vec());
    assert_eq!(
        source.log(&JobId("nope".into())).await.unwrap_err(),
        DataError::NotFound
    );
    let request = CancelRequest {
        actor: "dashboard:alex".into(),
        reason: Some("stuck".into()),
    };
    source.cancel(&id, request.clone()).await.unwrap();
    assert_eq!(source.cancels(), vec![(id, request)]);
}

#[tokio::test]
async fn unavailable_data_source_reports_error() {
    let source: FakeDataSource<(), ()> = FakeDataSource::new(());
    source.set_unavailable(Some("broker win-host down".into()));
    assert!(matches!(
        source.snapshot().await,
        Err(DataError::Unavailable(_))
    ));
}

#[tokio::test]
async fn kill_tree_without_start_fingerprint_is_refused() {
    let (_, clock) = clock();
    let runner = FakeProcessRunner::new(clock);
    let err = runner
        .kill_tree(&ProcessRef {
            pid: 5,
            start: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err, ProcessError::Unverifiable(5));
}
