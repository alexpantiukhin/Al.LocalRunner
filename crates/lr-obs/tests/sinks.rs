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
        accepting < std::time::Duration::from_secs(10),
        "{accepting:?}"
    );
    assert!(
        ready.load(Ordering::Relaxed) <= 50,
        "ready {}",
        ready.load(Ordering::Relaxed)
    );
    assert!(
        ticks.load(Ordering::Relaxed) <= 50,
        "ticks {}",
        ticks.load(Ordering::Relaxed)
    );
}

struct Counting {
    calls: Arc<std::sync::atomic::AtomicU64>,
}

impl lr_obs::queue::Writer for Counting {
    fn write(&mut self, _batch: &[lr_obs::redact::Redacted]) -> u64 {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        0
    }

    fn tick(&mut self) -> u64 {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        0
    }

    fn flush(&mut self) -> u64 {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        0
    }
}

#[test]
fn a_finished_queue_never_touches_its_writer_again() {
    use lr_obs::queue::{Queued, SinkKind};
    use std::sync::atomic::{AtomicU64, Ordering};
    let calls = Arc::new(AtomicU64::new(0));
    let meter: Arc<dyn Meter> = Arc::new(Metrics::new(T0, false));
    let q = Queued::new(
        SinkKind::File,
        Box::new(Counting {
            calls: calls.clone(),
        }),
        QueueLimits::default(),
        meter,
    );
    q.close();
    q.finish();
    let after_finish = calls.load(Ordering::Relaxed);
    let redactor = lr_obs::Redactor::new(&[]);
    q.accept(redactor.redact(&record("engine", Level::Info, "late")));
    q.flush();
    q.pump();
    q.finish();
    assert_eq!(calls.load(Ordering::Relaxed), after_finish);
    assert_eq!(q.dropped(), 1);
}

fn file_logger(fs: &lr_obs::fs::MemoryLogFs) -> lr_obs::Logger {
    use lr_abstractions::fakes::ManualClock;
    use lr_abstractions::Clock;
    use lr_obs::fs::LogFs;
    use lr_obs::logger::{ObsEnv, OtlpFactory, OtlpSettings, Resource};
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(T0));
    let otlp: OtlpFactory =
        Arc::new(|_: &OtlpSettings| Err(lr_obs::config::ConfigError("unused".into())));
    let env = ObsEnv {
        state_dir: std::path::PathBuf::from("state"),
        clock,
        fs: Arc::new(fs.clone()) as Arc<dyn LogFs>,
        console: None,
        os_log: None,
        otlp,
        metrics: Arc::new(Metrics::new(T0, false)),
        resource: Resource {
            role: Role::Broker,
            machine_id: "m-1".into(),
        },
        limits: QueueLimits::default(),
    };
    lr_obs::Logger::new(&file_config(), env).unwrap()
}

fn file_config() -> lr_obs::LoggingConfig {
    lr_obs::LoggingConfig::parse("[[sink]]\nkind = \"file\"\n").unwrap()
}

fn log_path() -> std::path::PathBuf {
    std::path::PathBuf::from("state")
        .join("logs")
        .join("daemon.log")
}

#[test]
fn reload_racing_flush_never_has_two_writers_on_one_path() {
    use lr_abstractions::LogSink;
    use lr_obs::fs::MemoryLogFs;
    use std::sync::atomic::{AtomicBool, Ordering};
    let fs = MemoryLogFs::new();
    let cfg = file_config();
    let logger = Arc::new(file_logger(&fs));
    let stop = Arc::new(AtomicBool::new(false));
    let busy: Vec<_> = (0..2)
        .map(|_| {
            let (logger, stop) = (logger.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    logger.write(&record("engine", Level::Info, &format!("f{n}")));
                    logger.flush();
                    n += 1;
                }
                n
            })
        })
        .collect();
    for _ in 0..200 {
        logger.reload(&cfg).unwrap();
        std::thread::yield_now();
    }
    stop.store(true, Ordering::Relaxed);
    let written: u64 = busy.into_iter().map(|t| t.join().unwrap()).sum();
    logger.shutdown();
    assert_eq!(fs.max_open(&log_path()), 1);
    let delivered = String::from_utf8(fs.file(&log_path()).unwrap_or_default())
        .unwrap()
        .lines()
        .count() as u64;
    assert_eq!(delivered + logger.dropped(), written);
}

#[test]
fn reload_after_shutdown_is_refused_and_starts_no_worker() {
    use lr_abstractions::LogSink;
    use lr_obs::fs::MemoryLogFs;
    let fs = MemoryLogFs::new();
    let logger = file_logger(&fs);
    logger.start_workers();
    logger.shutdown();
    assert!(logger.reload(&file_config()).is_err());
    let before = logger.dropped();
    logger.write(&record("engine", Level::Info, "after shutdown"));
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let file = String::from_utf8(fs.file(&log_path()).unwrap_or_default()).unwrap();
    assert!(!file.contains("after shutdown"), "{file}");
    assert_eq!(logger.dropped(), before + 1);
}
