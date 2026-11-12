use lr_abstractions::fakes::ManualClock;
use lr_abstractions::{Clock, Level, LogSink, Meter, Record, Role};
use lr_core::Timestamp;
use lr_obs::config::{ConfigError, LoggingConfig};
use lr_obs::fs::{LogFs, MemoryLogFs};
use lr_obs::logger::{Logger, ObsEnv, OtlpFactory, OtlpSettings, Resource, SharedWriter};
use lr_obs::metrics::{Metrics, Value};
use lr_obs::oslog::{MemoryOsLog, OsLogApi};
use lr_obs::otlp::{MemoryTransport, OtlpTransport};
use lr_obs::queue::QueueLimits;
use lr_obs::MASK;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const T0: Timestamp = Timestamp(1_700_000_000_000);

struct Bench {
    clock: ManualClock,
    fs: MemoryLogFs,
    console: Arc<Mutex<Vec<u8>>>,
    os_log: Arc<MemoryOsLog>,
    transport: Arc<MemoryTransport>,
    metrics: Arc<Metrics>,
}

impl Bench {
    fn new() -> Bench {
        Bench {
            clock: ManualClock::new(T0),
            fs: MemoryLogFs::new(),
            console: Arc::new(Mutex::new(Vec::new())),
            os_log: Arc::new(MemoryOsLog::new()),
            transport: Arc::new(MemoryTransport::new()),
            metrics: Arc::new(Metrics::new(T0, false)),
        }
    }

    fn env(&self, foreground: bool, limits: QueueLimits) -> ObsEnv {
        let transport = self.transport.clone();
        let otlp: OtlpFactory = Arc::new(move |_settings: &OtlpSettings| {
            let t: Arc<dyn OtlpTransport> = transport.clone();
            Ok(t)
        });
        let console: SharedWriter = self.console.clone();
        let os_log: Arc<dyn OsLogApi> = self.os_log.clone();
        let fs: Arc<dyn LogFs> = Arc::new(self.fs.clone());
        let clock: Arc<dyn Clock> = Arc::new(self.clock.clone());
        ObsEnv {
            state_dir: PathBuf::from("state"),
            clock,
            fs,
            console: foreground.then_some(console),
            os_log: Some(os_log),
            otlp,
            metrics: self.metrics.clone(),
            resource: Resource {
                role: Role::Broker,
                machine_id: "m-1".into(),
            },
            limits,
        }
    }

    fn logger(&self, toml: &str) -> Logger {
        let cfg = LoggingConfig::parse(toml).unwrap();
        Logger::new(&cfg, self.env(true, QueueLimits::default())).unwrap()
    }

    fn file(&self) -> String {
        let path = PathBuf::from("state").join("logs").join("daemon.log");
        String::from_utf8(self.fs.file(&path).unwrap_or_default()).unwrap()
    }

    fn console(&self) -> String {
        String::from_utf8(self.console.lock().unwrap().clone()).unwrap()
    }

    fn posted(&self) -> String {
        self.transport
            .posts()
            .into_iter()
            .map(|(_, body)| String::from_utf8(body).unwrap())
            .collect()
    }
}

const ALL_SINKS: &str = r#"
[[sink]]
kind = "file"
[[sink]]
kind = "console"
[[sink]]
kind = "journal"
[[sink]]
kind = "otlp"
endpoint = "https://alloy.lan:4318"
enabled = true
ca_file = "ca.pem"
"#;

fn record(component: &str, level: Level, message: &str) -> Record {
    Record::new(T0, level, Role::Broker, component, message)
}

#[test]
fn redaction_happens_before_every_sink() {
    let b = Bench::new();
    let logger = b.logger(ALL_SINKS);
    logger.redactor().protect("runtime-secret-1");
    logger.write(
        &record("engine", Level::Info, "saw runtime-secret-1 token=abc123")
            .field("password", "hunter2")
            .field("note", "lrj1.AAA.BBB.CCC"),
    );
    logger.flush();
    let outputs = [
        b.file(),
        b.console(),
        b.os_log.lines().join("\n"),
        b.posted(),
    ];
    for (n, out) in outputs.iter().enumerate() {
        assert!(out.contains(MASK), "sink {n}: {out}");
        for secret in ["runtime-secret-1", "abc123", "hunter2", "lrj1.AAA"] {
            assert!(!out.contains(secret), "sink {n} leaked {secret}: {out}");
        }
    }
}

#[test]
fn write_does_no_io_until_the_sinks_are_pumped() {
    let b = Bench::new();
    let logger = b.logger(ALL_SINKS);
    logger.write(&record("engine", Level::Info, "queued"));
    assert_eq!(b.file(), "");
    assert_eq!(b.console(), "");
    assert!(b.os_log.lines().is_empty());
    assert!(b.transport.posts().is_empty());
    logger.flush();
    assert!(b.file().contains("queued"));
}

#[test]
fn levels_filter_by_component() {
    let b = Bench::new();
    let logger = b.logger(
        "[logging]\nlevel = \"warn\"\n[logging.levels]\npolicy = \"debug\"\n[[sink]]\nkind = \"file\"\n",
    );
    logger.write(&record("engine", Level::Info, "engine-info"));
    logger.write(&record("engine", Level::Warn, "engine-warn"));
    logger.write(&record("policy", Level::Debug, "policy-debug"));
    logger.write(&record("policy", Level::Trace, "policy-trace"));
    logger.flush();
    let file = b.file();
    assert!(!file.contains("engine-info"));
    assert!(file.contains("engine-warn"));
    assert!(file.contains("policy-debug"));
    assert!(!file.contains("policy-trace"));
}

#[test]
fn console_only_in_the_foreground() {
    let b = Bench::new();
    let cfg = LoggingConfig::parse("[[sink]]\nkind = \"console\"\n").unwrap();
    let logger = Logger::new(&cfg, b.env(false, QueueLimits::default())).unwrap();
    logger.write(&record("engine", Level::Info, "background"));
    logger.flush();
    assert_eq!(b.console(), "");
}

#[test]
fn console_lines_neutralise_line_injection() {
    let b = Bench::new();
    let logger = b.logger("[[sink]]\nkind = \"console\"\n");
    logger.write(
        &record("engine", Level::Warn, "a\nFAKE ERROR\u{1b}[31m\u{202e}x")
            .field("host", "h 1\r\n")
            .field("bad name=", "v"),
    );
    logger.flush();
    let out = b.console();
    assert_eq!(
        out,
        concat!(
            "2023-11-14T22:13:20.000Z WARN  broker engine ",
            "\"a\\nFAKE ERROR\\u{1b}[31m\\u{202e}x\" host=\"h 1\\r\\n\" bad_name_=v\n"
        )
    );
}

#[test]
fn journal_gets_level_and_text_without_time() {
    let b = Bench::new();
    let logger = b.logger("[[sink]]\nkind = \"journal\"\n");
    logger.write(&record("engine", Level::Error, "boom").field("job_id", "j1"));
    logger.flush();
    assert_eq!(
        b.os_log.entries(),
        vec![(Level::Error, "broker engine \"boom\" job_id=j1".to_string())]
    );
}

#[test]
fn journal_without_os_support_is_rejected() {
    let b = Bench::new();
    let cfg = LoggingConfig::parse("[[sink]]\nkind = \"journal\"\n").unwrap();
    let mut env = b.env(true, QueueLimits::default());
    env.os_log = None;
    let e: ConfigError = Logger::new(&cfg, env).err().unwrap();
    assert!(e.to_string().contains("journal"));
}

#[test]
fn queue_limits_default_to_exact_values() {
    let limits = QueueLimits::default();
    assert_eq!(limits.max_records, 10_000);
    assert_eq!(limits.max_bytes, 8 * 1024 * 1024);
}

#[test]
fn full_buffer_drops_oldest_and_counts_per_sink() {
    let b = Bench::new();
    let cfg =
        LoggingConfig::parse("[[sink]]\nkind = \"file\"\n[[sink]]\nkind = \"console\"\n").unwrap();
    let limits = QueueLimits {
        max_records: 2,
        max_bytes: 1 << 20,
    };
    let logger = Logger::new(&cfg, b.env(true, limits)).unwrap();
    for n in 0..5 {
        logger.write(&record("engine", Level::Info, &format!("m{n}")));
    }
    assert_eq!(logger.dropped(), 6);
    logger.flush();
    let file = b.file();
    assert!(!file.contains("\"m2\""));
    assert!(file.contains("\"m3\"") && file.contains("\"m4\""));
    let dropped: Vec<(String, Value)> = b
        .metrics
        .snapshot()
        .into_iter()
        .filter(|p| p.name == "localrunner_log_dropped_total")
        .map(|p| (p.labels[0].1.clone(), p.value))
        .collect();
    assert_eq!(
        dropped,
        vec![
            ("console".to_string(), Value::Counter(3)),
            ("file".to_string(), Value::Counter(3))
        ]
    );
}

#[test]
fn byte_limit_bounds_the_queue_too() {
    let b = Bench::new();
    let cfg = LoggingConfig::parse("[[sink]]\nkind = \"file\"\n").unwrap();
    let limits = QueueLimits {
        max_records: 1000,
        max_bytes: 3000,
    };
    let logger = Logger::new(&cfg, b.env(true, limits)).unwrap();
    for _ in 0..10 {
        logger.write(&record("engine", Level::Info, &"x".repeat(1000)));
    }
    assert!(logger.dropped() >= 7);
}

#[test]
fn io_errors_are_counted_and_lost_events_dropped() {
    let b = Bench::new();
    let logger = b.logger("[[sink]]\nkind = \"file\"\n");
    b.fs.fail(Some("disk gone"));
    logger.write(&record("engine", Level::Info, "lost"));
    logger.flush();
    assert_eq!(logger.errors(), 1);
    assert_eq!(logger.dropped(), 1);
}

#[test]
fn reload_switches_configuration_and_keeps_protected_values() {
    let b = Bench::new();
    let logger = b.logger("[logging]\nlevel = \"error\"\n[[sink]]\nkind = \"file\"\n");
    logger.redactor().protect("kept-secret");
    logger.write(&record("engine", Level::Info, "before"));
    let cfg = LoggingConfig::parse(
        "[logging]\nlevel = \"info\"\n[[sink]]\nkind = \"file\"\n[redact]\nfields = [\"ssn\"]\n",
    )
    .unwrap();
    logger.reload(&cfg).unwrap();
    logger.write(&record("engine", Level::Info, "after kept-secret").field("ssn", "123"));
    logger.flush();
    let file = b.file();
    assert!(!file.contains("before"));
    assert!(file.contains("after"));
    assert!(!file.contains("kept-secret"));
    assert!(!file.contains("123"));
}

#[test]
fn failed_reload_keeps_the_previous_configuration() {
    let b = Bench::new();
    let cfg = LoggingConfig::parse(
        "[[sink]]
kind = \"file\"
",
    )
    .unwrap();
    let mut env = b.env(true, QueueLimits::default());
    env.os_log = None;
    let logger = Logger::new(&cfg, env).unwrap();
    let bad = LoggingConfig::parse(
        "[logging]
level = \"error\"
[[sink]]
kind = \"journal\"
",
    )
    .unwrap();
    assert!(logger.reload(&bad).is_err());
    logger.write(&record("engine", Level::Info, "still-file"));
    logger.flush();
    assert!(b.file().contains("still-file"));
}

#[test]
fn workers_deliver_in_the_background_and_shutdown_flushes() {
    let b = Bench::new();
    let logger = b.logger("[[sink]]\nkind = \"file\"\n");
    logger.start_workers();
    for n in 0..50 {
        logger.write(&record("engine", Level::Info, &format!("bg{n}")));
    }
    logger.shutdown();
    let file = b.file();
    assert!(file.contains("\"bg0\"") && file.contains("\"bg49\""));
    assert!(b.fs.flushes() >= 1);
    logger.write(&record("engine", Level::Info, "after-shutdown"));
    logger.flush();
    assert!(!b.file().contains("after-shutdown"));
}

#[test]
fn metrics_stay_in_memory_without_an_enabled_receiver() {
    let b = Bench::new();
    let logger = b.logger(
        "[[sink]]\nkind = \"otlp\"\nendpoint = \"https://h\"\nenabled = false\n[[sink]]\nkind = \"file\"\n",
    );
    b.metrics
        .set("localrunner_queue_length", &[("platform", "p")], 1.0);
    b.clock.advance(std::time::Duration::from_secs(120));
    logger.write(&record("engine", Level::Info, "m"));
    logger.flush();
    assert!(b.transport.posts().is_empty());
}

#[test]
fn client_label_setting_reaches_the_metrics() {
    let b = Bench::new();
    let logger = b.logger("[metrics]\nclient_label = true\n");
    assert!(std::sync::Arc::ptr_eq(&logger.metrics(), &b.metrics));
    b.metrics.add(
        "localrunner_orders_total",
        &[("result", "passed"), ("platform", "p"), ("client", "ci")],
        1,
    );
    assert_eq!(b.metrics.snapshot()[0].labels.len(), 3);
}

#[test]
fn os_journal_levels_map_to_os_priorities() {
    use lr_obs::oslog::{event_type, syslog_priority};
    let levels = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
    ];
    let syslog: Vec<i32> = levels.iter().map(|l| syslog_priority(*l)).collect();
    assert_eq!(syslog, vec![7, 7, 6, 4, 3]);
    let windows: Vec<u16> = levels.iter().map(|l| event_type(*l)).collect();
    assert_eq!(windows, vec![4, 4, 4, 2, 1]);
}

#[test]
fn otlp_settings_are_resolved_against_the_state_directory() {
    let b = Bench::new();
    let seen: Arc<Mutex<Vec<OtlpSettings>>> = Arc::new(Mutex::new(Vec::new()));
    let mut env = b.env(true, QueueLimits::default());
    let (sink, transport) = (seen.clone(), b.transport.clone());
    env.otlp = Arc::new(move |s: &OtlpSettings| {
        sink.lock().unwrap().push(s.clone());
        let t: Arc<dyn OtlpTransport> = transport.clone();
        Ok(t)
    });
    let cfg = LoggingConfig::parse(
        "[[sink]]\nkind = \"otlp\"\nendpoint = \"https://alloy.lan:4318/base\"\nenabled = true\nca_file = \"certs/ca.pem\"\n",
    )
    .unwrap();
    let _logger = Logger::new(&cfg, env).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].ca_file, PathBuf::from("state").join("certs/ca.pem"));
    assert_eq!(seen[0].endpoint.host, "alloy.lan");
    assert_eq!(seen[0].endpoint.port, 4318);
    assert_eq!(seen[0].endpoint.path, "/base");
    assert_eq!(seen[0].timeout, std::time::Duration::from_secs(10));
}

struct Stuck {
    release: Mutex<bool>,
    wake: std::sync::Condvar,
}

impl OtlpTransport for Stuck {
    fn post(&self, _path: &str, _body: &[u8]) -> Result<(), lr_obs::otlp::ExportError> {
        let mut open = self.release.lock().unwrap();
        while !*open {
            open = self.wake.wait(open).unwrap();
        }
        Ok(())
    }
}

#[test]
fn unreachable_receiver_never_blocks_writers() {
    let b = Bench::new();
    let stuck = Arc::new(Stuck {
        release: Mutex::new(false),
        wake: std::sync::Condvar::new(),
    });
    let limits = QueueLimits {
        max_records: 100,
        max_bytes: 1 << 20,
    };
    let mut env = b.env(true, limits);
    let t = stuck.clone();
    env.otlp = Arc::new(move |_: &OtlpSettings| Ok(t.clone() as Arc<dyn OtlpTransport>));
    let cfg = LoggingConfig::parse(
        "[[sink]]\nkind = \"otlp\"\nendpoint = \"https://h\"\nenabled = true\nca_file = \"ca.pem\"\n[[sink]]\nkind = \"file\"\n",
    )
    .unwrap();
    let logger = Logger::new(&cfg, env).unwrap();
    logger.start_workers();
    for n in 0..5_000 {
        logger.write(&record("engine", Level::Info, &format!("w{n}")));
    }
    assert!(logger.dropped() >= 4_000);
    *stuck.release.lock().unwrap() = true;
    stuck.wake.notify_all();
    logger.shutdown();
    assert!(b.file().contains("\"w4999\""));
}

#[test]
fn text_message_cannot_forge_a_field() {
    let b = Bench::new();
    let logger = b.logger("[[sink]]\nkind = \"journal\"\n");
    logger.write(&record("engine", Level::Info, "ok\" job_id=evil").field("path", "C:\\dir \"x\""));
    logger.flush();
    assert_eq!(
        b.os_log.lines(),
        vec!["broker engine \"ok\\\" job_id=evil\" path=\"C:\\\\dir \\\"x\\\"\"".to_string()]
    );
}

#[test]
fn reload_never_has_two_writers_on_one_path() {
    let b = Bench::new();
    let logger = Arc::new(b.logger("[[sink]]\nkind = \"file\"\n"));
    logger.start_workers();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (logger, stop) = (logger.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                logger.write(&record("engine", Level::Info, &format!("r{n}")));
                n += 1;
                if n.is_multiple_of(64) {
                    std::thread::yield_now();
                }
            }
        })
    };
    let cfg = LoggingConfig::parse("[[sink]]\nkind = \"file\"\n").unwrap();
    for _ in 0..30 {
        logger.reload(&cfg).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
    logger.shutdown();
    let path = PathBuf::from("state").join("logs").join("daemon.log");
    assert_eq!(b.fs.max_open(&path), 1);
}
