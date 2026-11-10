use lr_abstractions::{Level, Meter, Record, Role};
use lr_core::Timestamp;
use lr_obs::metrics::Metrics;
use lr_obs::queue::QueueLimits;
use std::sync::Arc;

const T0: Timestamp = Timestamp(1_700_000_000_000);

fn record(component: &str, level: Level, message: &str) -> Record {
    Record::new(T0, level, Role::Broker, component, message)
}

#[test]
fn journal_text_is_cut_to_the_event_log_limit() {
    use lr_obs::oslog::{event_text, EVENT_TEXT_MAX};
    assert_eq!(EVENT_TEXT_MAX, 31_839);
    let long = "é".repeat(EVENT_TEXT_MAX + 10);
    let cut = event_text(&long);
    assert_eq!(cut.encode_utf16().count(), EVENT_TEXT_MAX);
    assert!(cut.ends_with("...[truncated]"));
    let wide = "😀".repeat(EVENT_TEXT_MAX);
    assert!(event_text(&wide).encode_utf16().count() <= EVENT_TEXT_MAX);
    assert_eq!(event_text("short"), "short");
}

struct Paused {
    ready: Arc<std::sync::atomic::AtomicU64>,
    ticks: Arc<std::sync::atomic::AtomicU64>,
}

impl lr_obs::queue::Writer for Paused {
    fn write(&mut self, _batch: &[lr_obs::redact::Redacted]) -> u64 {
        0
    }

    fn ready(&mut self) -> bool {
        self.ready
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        false
    }

    fn tick(&mut self) -> u64 {
        self.ticks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        0
    }
}

#[test]
fn paused_writer_does_not_spin_the_worker() {
    use lr_obs::queue::{Queued, SinkKind};
    use std::sync::atomic::{AtomicU64, Ordering};
    let ready = Arc::new(AtomicU64::new(0));
    let ticks = Arc::new(AtomicU64::new(0));
    let writer = Paused {
        ready: ready.clone(),
        ticks: ticks.clone(),
    };
    let meter: Arc<dyn Meter> = Arc::new(Metrics::new(T0, false));
    let q = Arc::new(Queued::new(
        SinkKind::Otlp,
        Box::new(writer),
        QueueLimits::default(),
        meter,
    ));
    let worker = {
        let q = q.clone();
        std::thread::spawn(move || q.run())
    };
    let redactor = lr_obs::Redactor::new(&[]);
    let started = std::time::Instant::now();
    for n in 0..20_000 {
        q.accept(redactor.redact(&record("engine", Level::Info, &format!("m{n}"))));
    }
    let accepting = started.elapsed();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    q.close();
    worker.join().unwrap();
    assert!(
        accepting < std::time::Duration::from_secs(2),
        "{accepting:?}"
    );
    assert!(
        ready.load(Ordering::Relaxed) <= 10,
        "ready {}",
        ready.load(Ordering::Relaxed)
    );
    assert!(
        ticks.load(Ordering::Relaxed) <= 10,
        "ticks {}",
        ticks.load(Ordering::Relaxed)
    );
}
