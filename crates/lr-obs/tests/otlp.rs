use lr_abstractions::fakes::ManualClock;
use lr_abstractions::{Clock, Level, Meter, Record, Role};
use lr_core::Timestamp;
use lr_obs::config::{parse_endpoint, Signals};
use lr_obs::https::HttpsTransport;
use lr_obs::logger::Resource;
use lr_obs::metrics::Metrics;
use lr_obs::otlp::{
    severity_number, ExportError, MemoryTransport, OtlpTransport, OtlpWriter, BACKOFF_MAX_MS,
    BACKOFF_START_MS, BATCH_MAX, LOGS_PATH, METRICS_PATH, METRICS_PERIOD_MS,
};
use lr_obs::otlp_json::{encode_logs, encode_metrics};
use lr_obs::queue::{QueueLimits, Queued, SinkKind, Writer};
use lr_obs::redact::{Redacted, Redactor};
use serde_json::Value as Json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

const T0: Timestamp = Timestamp(1_700_000_000_000);

fn resource() -> Resource {
    Resource {
        role: Role::Runner,
        machine_id: "m-7".into(),
    }
}

fn redacted(message: &str) -> Redacted {
    let r = Record::new(T0, Level::Info, Role::Runner, "agent", message).field("job_id", "j1");
    Redactor::new(&[]).redact(&r)
}

fn both() -> Signals {
    Signals {
        logs: true,
        metrics: true,
    }
}

struct Bench {
    clock: ManualClock,
    transport: Arc<MemoryTransport>,
    metrics: Arc<Metrics>,
}

impl Bench {
    fn new() -> Bench {
        Bench {
            clock: ManualClock::new(T0),
            transport: Arc::new(MemoryTransport::new()),
            metrics: Arc::new(Metrics::new(T0, false)),
        }
    }

    fn writer(&self, signals: Signals) -> OtlpWriter {
        let t: Arc<dyn OtlpTransport> = self.transport.clone();
        let c: Arc<dyn Clock> = Arc::new(self.clock.clone());
        OtlpWriter::new(t, c, signals, self.metrics.clone(), resource())
    }

    fn advance(&self, ms: u64) {
        self.clock.advance(Duration::from_millis(ms));
    }
}

#[test]
fn constants_are_exact() {
    assert_eq!(LOGS_PATH, "/v1/logs");
    assert_eq!(METRICS_PATH, "/v1/metrics");
    assert_eq!(METRICS_PERIOD_MS, 60_000);
    assert_eq!(BACKOFF_START_MS, 5_000);
    assert_eq!(BACKOFF_MAX_MS, 300_000);
    assert_eq!(BATCH_MAX, 512);
}

#[test]
fn severity_numbers_follow_otlp() {
    assert_eq!(severity_number(Level::Trace), 1);
    assert_eq!(severity_number(Level::Debug), 5);
    assert_eq!(severity_number(Level::Info), 9);
    assert_eq!(severity_number(Level::Warn), 13);
    assert_eq!(severity_number(Level::Error), 17);
}

#[test]
fn logs_are_encoded_as_otlp_json() {
    let body: Json =
        serde_json::from_slice(&encode_logs(&resource(), &[redacted("hello")])).unwrap();
    let rl = &body["resourceLogs"][0];
    let attrs = rl["resource"]["attributes"].as_array().unwrap();
    let attr = |k: &str| {
        attrs
            .iter()
            .find(|a| a["key"] == k)
            .map(|a| a["value"]["stringValue"].as_str().unwrap().to_string())
    };
    assert_eq!(attr("service.name").as_deref(), Some("localrunner"));
    assert_eq!(attr("localrunner.role").as_deref(), Some("runner"));
    assert_eq!(attr("localrunner.machine_id").as_deref(), Some("m-7"));
    let scope = &rl["scopeLogs"][0];
    assert_eq!(scope["scope"]["name"], "localrunner");
    let log = &scope["logRecords"][0];
    assert_eq!(log["timeUnixNano"], "1700000000000000000");
    assert_eq!(log["severityNumber"], 9);
    assert_eq!(log["severityText"], "INFO");
    assert_eq!(log["body"]["stringValue"], "hello");
    let la = log["attributes"].as_array().unwrap();
    assert!(la
        .iter()
        .any(|a| a["key"] == "component" && a["value"]["stringValue"] == "agent"));
    assert!(la
        .iter()
        .any(|a| a["key"] == "job_id" && a["value"]["stringValue"] == "j1"));
}

#[test]
fn metrics_are_encoded_as_cumulative_otlp_json() {
    let m = Metrics::new(T0, false);
    m.add(
        "localrunner_orders_total",
        &[("result", "passed"), ("platform", "p")],
        3,
    );
    m.set("localrunner_queue_length", &[("platform", "p")], 2.5);
    m.observe("localrunner_queue_wait_seconds", &[("platform", "p")], 7.0);
    let now = Timestamp(T0.0 + 1000);
    let body: Json = serde_json::from_slice(&encode_metrics(&resource(), &m, now)).unwrap();
    let list = body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
        .as_array()
        .unwrap();
    let find = |n: &str| list.iter().find(|x| x["name"] == n).unwrap().clone();
    let gauge = find("localrunner_queue_length");
    assert_eq!(gauge["unit"], "{order}");
    assert_eq!(gauge["gauge"]["dataPoints"][0]["asDouble"], 2.5);
    assert_eq!(
        gauge["gauge"]["dataPoints"][0]["timeUnixNano"],
        "1700000001000000000"
    );
    let sum = find("localrunner_orders_total");
    assert_eq!(sum["sum"]["isMonotonic"], true);
    assert_eq!(sum["sum"]["aggregationTemporality"], 2);
    let point = &sum["sum"]["dataPoints"][0];
    assert_eq!(point["asInt"], "3");
    assert_eq!(point["startTimeUnixNano"], "1700000000000000000");
    let attrs = point["attributes"].as_array().unwrap();
    assert_eq!(attrs.len(), 2);
    assert_eq!(attrs[0]["key"], "result");
    let hist = find("localrunner_queue_wait_seconds");
    let hp = &hist["histogram"]["dataPoints"][0];
    assert_eq!(hist["histogram"]["aggregationTemporality"], 2);
    assert_eq!(hp["count"], "1");
    assert_eq!(hp["sum"], 7.0);
    assert_eq!(hp["explicitBounds"].as_array().unwrap().len(), 12);
    let buckets: Vec<&str> = hp["bucketCounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(buckets.len(), 13);
    assert_eq!(buckets[2], "1");
}

#[test]
fn failed_export_drops_the_batch_and_backs_off_exponentially() {
    let b = Bench::new();
    let mut w = b.writer(both());
    b.transport.set_failing(true);
    assert!(w.ready());
    assert_eq!(w.write(&[redacted("a"), redacted("b")]), 2);
    assert_eq!(b.transport.attempts(), 1);
    let mut wait = BACKOFF_START_MS;
    for _ in 0..8 {
        b.advance(wait - 1);
        assert!(!w.ready(), "ready before {wait} ms");
        b.advance(1);
        assert!(w.ready(), "not ready after {wait} ms");
        assert_eq!(w.write(&[redacted("c")]), 1);
        wait = (wait * 2).min(BACKOFF_MAX_MS);
    }
    assert_eq!(wait, BACKOFF_MAX_MS);
    b.transport.set_failing(false);
    b.advance(BACKOFF_MAX_MS);
    assert_eq!(w.write(&[redacted("d")]), 0);
    b.transport.set_failing(true);
    w.write(&[redacted("e")]);
    b.advance(BACKOFF_START_MS - 1);
    assert!(!w.ready());
    b.advance(1);
    assert!(w.ready());
}

#[test]
fn logs_go_to_the_logs_path_in_batches_of_at_most_512() {
    let b = Bench::new();
    let w = b.writer(both());
    let meter: Arc<dyn Meter> = b.metrics.clone();
    let q = Queued::new(SinkKind::Otlp, Box::new(w), QueueLimits::default(), meter);
    for n in 0..(BATCH_MAX + 1) {
        q.accept(redacted(&format!("m{n}")));
    }
    q.pump();
    let posts = b.transport.posts();
    assert_eq!(posts.len(), 2);
    assert!(posts.iter().all(|(p, _)| p == LOGS_PATH));
    let count = |body: &[u8]| {
        let j: Json = serde_json::from_slice(body).unwrap();
        j["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(&posts[0].1), BATCH_MAX);
    assert_eq!(count(&posts[1].1), 1);
}

#[test]
fn queue_drops_oldest_with_counter() {
    let b = Bench::new();
    let w = b.writer(both());
    let meter: Arc<dyn Meter> = b.metrics.clone();
    let limits = QueueLimits {
        max_records: 3,
        max_bytes: 1 << 20,
    };
    let q = Queued::new(SinkKind::Otlp, Box::new(w), limits, meter);
    for n in 0..5 {
        q.accept(redacted(&format!("m{n}")));
    }
    assert_eq!(q.dropped(), 2);
    q.pump();
    let body = String::from_utf8(b.transport.posts()[0].1.clone()).unwrap();
    assert!(!body.contains("\"m1\"") && body.contains("\"m2\"") && body.contains("\"m4\""));
    let snap = b.metrics.snapshot();
    assert_eq!(
        snap[0].labels,
        vec![("sink".to_string(), "otlp".to_string())]
    );
}

#[test]
fn metrics_are_exported_every_period_only_when_enabled() {
    let b = Bench::new();
    let mut w = b.writer(both());
    b.metrics
        .set("localrunner_queue_length", &[("platform", "p")], 1.0);
    w.tick();
    assert!(b.transport.posts().is_empty());
    b.advance(METRICS_PERIOD_MS - 1);
    w.tick();
    assert!(b.transport.posts().is_empty());
    b.advance(1);
    w.tick();
    w.tick();
    assert_eq!(b.transport.posts().len(), 1);
    assert_eq!(b.transport.posts()[0].0, METRICS_PATH);
    b.advance(METRICS_PERIOD_MS);
    w.tick();
    assert_eq!(b.transport.posts().len(), 2);

    let only_logs = Bench::new();
    let mut w = only_logs.writer(Signals {
        logs: true,
        metrics: false,
    });
    only_logs.advance(10 * METRICS_PERIOD_MS);
    w.tick();
    assert!(only_logs.transport.posts().is_empty());
}

#[test]
fn logs_are_not_sent_when_only_metrics_are_enabled() {
    let b = Bench::new();
    let mut w = b.writer(Signals {
        logs: false,
        metrics: true,
    });
    assert_eq!(w.write(&[redacted("x")]), 0);
    assert!(b.transport.posts().is_empty());
}

struct Pki {
    ca_pem: String,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
}

fn pki() -> Pki {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &ca, &ca_key).unwrap();
    Pki {
        ca_pem: ca.pem(),
        cert: cert.der().clone(),
        key: rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    }
}

fn serve_once(pki: &Pki, status: &'static str) -> (u16, std::thread::JoinHandle<Vec<u8>>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![pki.cert.clone()], pki.key.clone_key())
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tls.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
            }
            if let Some(end) = find(&got, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&got[..end]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map(|v| v.trim().parse().unwrap())
                    .unwrap_or(0);
                if got.len() >= end + 4 + len {
                    break;
                }
            }
        }
        let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let _ = tls.write_all(reply.as_bytes());
        let _ = tls.flush();
        tls.conn.send_close_notify();
        let _ = tls.flush();
        got
    });
    (port, handle)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn https_transport_posts_json_over_verified_tls() {
    let pki = pki();
    let (port, server) = serve_once(&pki, "200 OK");
    let endpoint = parse_endpoint(&format!("https://localhost:{port}/otlp")).unwrap();
    let t = HttpsTransport::new(&endpoint, pki.ca_pem.as_bytes(), Duration::from_secs(5)).unwrap();
    t.post(LOGS_PATH, b"{\"a\":1}").unwrap();
    let got = String::from_utf8(server.join().unwrap()).unwrap();
    assert!(got.starts_with("POST /otlp/v1/logs HTTP/1.1\r\n"), "{got}");
    assert!(got.contains(&format!("Host: localhost:{port}\r\n")));
    assert!(got.contains("Content-Type: application/json\r\n"));
    assert!(got.contains("Content-Length: 7\r\n"));
    assert!(got.ends_with("\r\n\r\n{\"a\":1}"));
}

#[test]
fn https_transport_reports_http_status() {
    let pki = pki();
    let (port, server) = serve_once(&pki, "503 Service Unavailable");
    let endpoint = parse_endpoint(&format!("https://localhost:{port}")).unwrap();
    let t = HttpsTransport::new(&endpoint, pki.ca_pem.as_bytes(), Duration::from_secs(5)).unwrap();
    assert_eq!(t.post(METRICS_PATH, b"{}"), Err(ExportError::Status(503)));
    server.join().unwrap();
}

#[test]
fn https_transport_refuses_a_server_outside_the_configured_ca() {
    let trusted = pki();
    let other = pki();
    let (port, server) = serve_once(&other, "200 OK");
    let endpoint = parse_endpoint(&format!("https://localhost:{port}")).unwrap();
    let t =
        HttpsTransport::new(&endpoint, trusted.ca_pem.as_bytes(), Duration::from_secs(5)).unwrap();
    assert!(matches!(t.post(LOGS_PATH, b"{}"), Err(ExportError::Io(_))));
    let _ = server.join();
}

#[test]
fn https_transport_times_out_on_a_silent_receiver() {
    let pki = pki();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hold = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_secs(3));
        drop(tcp);
    });
    let endpoint = parse_endpoint(&format!("https://localhost:{port}")).unwrap();
    let t =
        HttpsTransport::new(&endpoint, pki.ca_pem.as_bytes(), Duration::from_millis(300)).unwrap();
    let started = Instant::now();
    assert!(t.post(LOGS_PATH, b"{}").is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    hold.join().unwrap();
}

#[test]
fn https_transport_rejects_a_bad_ca_file() {
    let endpoint = parse_endpoint("https://localhost:4318").unwrap();
    assert!(HttpsTransport::new(&endpoint, b"not a pem", Duration::from_secs(1)).is_err());
}

#[test]
fn https_transport_refuses_a_trusted_certificate_for_another_host() {
    let pki = pki();
    let (port, server) = serve_once(&pki, "200 OK");
    let endpoint = parse_endpoint(&format!("https://127.0.0.1:{port}")).unwrap();
    let t = HttpsTransport::new(&endpoint, pki.ca_pem.as_bytes(), Duration::from_secs(5)).unwrap();
    assert!(matches!(t.post(LOGS_PATH, b"{}"), Err(ExportError::Io(_))));
    let _ = server.join();
}
