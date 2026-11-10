use lr_abstractions::Level;
use lr_obs::config::{
    parse_endpoint, Endpoint, FileSinkConfig, LoggingConfig, OtlpSinkConfig, Signals, SinkConfig,
    DEFAULT_FILE_PATH, DEFAULT_KEEP_DAYS, DEFAULT_KEEP_FILES, DEFAULT_OTLP_TIMEOUT_S,
    DEFAULT_ROTATE_MB,
};
use lr_obs::format::Format;
use std::path::PathBuf;

const SPEC: &str = include_str!("../../../docs/architecture.html");

fn spec_example() -> &'static str {
    let start = SPEC.find("<pre class=\"code\">[logging]").unwrap() + "<pre class=\"code\">".len();
    let end = start + SPEC[start..].find("</pre>").unwrap();
    &SPEC[start..end]
}

fn default_file() -> FileSinkConfig {
    FileSinkConfig {
        path: PathBuf::from("logs/daemon.log"),
        format: Format::Json,
        rotate_mb: 50,
        keep_days: 14,
        keep_files: 10,
    }
}

fn err(text: &str) -> String {
    LoggingConfig::parse(text).unwrap_err().to_string()
}

#[test]
fn defaults_are_exact() {
    assert_eq!(DEFAULT_FILE_PATH, "logs/daemon.log");
    assert_eq!(DEFAULT_ROTATE_MB, 50);
    assert_eq!(DEFAULT_KEEP_DAYS, 14);
    assert_eq!(DEFAULT_KEEP_FILES, 10);
    assert_eq!(DEFAULT_OTLP_TIMEOUT_S, 10);
}

#[test]
fn without_file_default_is_rotated_json_file_at_info() {
    let cfg = LoggingConfig::load(None).unwrap();
    assert_eq!(cfg.level, Level::Info);
    assert!(cfg.levels.is_empty());
    assert_eq!(cfg.sinks, vec![SinkConfig::File(default_file())]);
    assert!(cfg.redact_fields.is_empty());
    assert!(!cfg.client_label);
    assert_eq!(cfg, LoggingConfig::default());
}

#[test]
fn spec_example_parses() {
    let cfg = LoggingConfig::load(Some(spec_example())).unwrap();
    assert_eq!(cfg.level, Level::Info);
    assert_eq!(cfg.levels.get("policy"), Some(&Level::Debug));
    assert_eq!(cfg.level_for("policy"), Level::Debug);
    assert_eq!(cfg.level_for("engine"), Level::Info);
    assert_eq!(
        cfg.sinks,
        vec![
            SinkConfig::File(default_file()),
            SinkConfig::Console,
            SinkConfig::Otlp(OtlpSinkConfig {
                endpoint: "https://alloy.lan:4318".into(),
                signals: Signals {
                    logs: true,
                    metrics: true
                },
                enabled: false,
                ca_file: Some(PathBuf::from("certs/alloy-ca.pem")),
                timeout_s: 10,
            }),
        ]
    );
    assert_eq!(cfg.redact_fields, vec!["command", "env", "token"]);
}

#[test]
fn file_sink_keys_default_individually() {
    let cfg = LoggingConfig::parse("[[sink]]\nkind = \"file\"\n").unwrap();
    assert_eq!(cfg.sinks, vec![SinkConfig::File(default_file())]);
    let cfg = LoggingConfig::parse(
        "[[sink]]\nkind = \"file\"\npath = \"x/a.log\"\nformat = \"text\"\nrotate_mb = 5\nkeep_days = 2\nkeep_files = 3\n",
    )
    .unwrap();
    assert_eq!(
        cfg.sinks,
        vec![SinkConfig::File(FileSinkConfig {
            path: PathBuf::from("x/a.log"),
            format: Format::Text,
            rotate_mb: 5,
            keep_days: 2,
            keep_files: 3,
        })]
    );
}

#[test]
fn file_without_sinks_uses_the_default_file_sink() {
    let cfg = LoggingConfig::parse("[logging]\nlevel = \"warn\"\n").unwrap();
    assert_eq!(cfg.level, Level::Warn);
    assert_eq!(cfg.sinks, vec![SinkConfig::File(default_file())]);
}

#[test]
fn journal_sink_and_metrics_client_label_parse() {
    let cfg =
        LoggingConfig::parse("[[sink]]\nkind = \"journal\"\n[metrics]\nclient_label = true\n")
            .unwrap();
    assert_eq!(cfg.sinks, vec![SinkConfig::Journal]);
    assert!(cfg.client_label);
}

#[test]
fn every_level_name_parses() {
    for (name, level) in [
        ("trace", Level::Trace),
        ("debug", Level::Debug),
        ("info", Level::Info),
        ("warn", Level::Warn),
        ("error", Level::Error),
    ] {
        let cfg = LoggingConfig::parse(&format!("[logging]\nlevel = \"{name}\"\n")).unwrap();
        assert_eq!(cfg.level, level);
    }
    assert!(err("[logging]\nlevel = \"loud\"\n").contains("loud"));
}

#[test]
fn unknown_keys_and_kinds_are_rejected() {
    assert!(err("[logging]\nlevle = \"info\"\n").contains("levle"));
    assert!(err("[[sink]]\nkind = \"syslog\"\n").contains("syslog"));
    assert!(err("[[sink]]\nkind = \"console\"\npath = \"x\"\n").contains("path"));
    assert!(err("[[sink]]\nkind = \"file\"\nformat = \"xml\"\n").contains("xml"));
    assert!(
        err("[[sink]]\nkind = \"otlp\"\nendpoint = \"https://h\"\nsignals = [\"traces\"]\n")
            .contains("traces")
    );
}

#[test]
fn file_path_must_stay_inside_the_state_directory() {
    for path in [
        "../x.log",
        "logs/../../x.log",
        "/abs.log",
        "C:\\\\x.log",
        "C:x.log",
        "\\\\\\\\server\\\\share\\\\x.log",
        "logs/con.log",
        "logs/NUL",
        "logs/a:b.log",
        "logs/a*b.log",
        "logs/a.log.",
        "logs/",
        "",
        "./",
    ] {
        let text = format!("[[sink]]\nkind = \"file\"\npath = \"{path}\"\n");
        assert!(LoggingConfig::parse(&text).is_err(), "{path} accepted");
    }
    for path in ["daemon.log", "logs/daemon.log", "./logs/x-1.log"] {
        let text = format!("[[sink]]\nkind = \"file\"\npath = \"{path}\"\n");
        assert!(LoggingConfig::parse(&text).is_ok(), "{path} refused");
    }
}

#[test]
fn rotation_values_must_be_positive() {
    for key in ["rotate_mb", "keep_days", "keep_files"] {
        let text = format!("[[sink]]\nkind = \"file\"\n{key} = 0\n");
        assert!(err(&text).contains(key), "{key}");
    }
}

#[test]
fn enabled_otlp_needs_ca_file_and_valid_timeout() {
    let base = "[[sink]]\nkind = \"otlp\"\nendpoint = \"https://alloy.lan:4318\"\nenabled = true\n";
    assert!(err(base).contains("ca_file"));
    let ok = LoggingConfig::parse(&format!("{base}ca_file = \"ca.pem\"\ntimeout_s = 3\n")).unwrap();
    match &ok.sinks[0] {
        SinkConfig::Otlp(o) => {
            assert_eq!(o.ca_file, Some(PathBuf::from("ca.pem")));
            assert_eq!(o.timeout_s, 3);
        }
        other => panic!("{other:?}"),
    }
    assert!(err(&format!("{base}ca_file = \"ca.pem\"\ntimeout_s = 0\n")).contains("timeout_s"));
}

#[test]
fn otlp_signals_default_to_logs_and_metrics_and_must_not_be_empty() {
    let cfg =
        LoggingConfig::parse("[[sink]]\nkind = \"otlp\"\nendpoint = \"https://h\"\n").unwrap();
    match &cfg.sinks[0] {
        SinkConfig::Otlp(o) => {
            assert_eq!(
                o.signals,
                Signals {
                    logs: true,
                    metrics: true
                }
            );
            assert!(!o.enabled);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        err("[[sink]]\nkind = \"otlp\"\nendpoint = \"https://h\"\nsignals = []\n")
            .contains("signals")
    );
}

#[test]
fn otlp_endpoint_is_https_without_credentials() {
    assert_eq!(
        parse_endpoint("https://alloy.lan:4318").unwrap(),
        Endpoint {
            host: "alloy.lan".into(),
            port: 4318,
            path: String::new()
        }
    );
    assert_eq!(
        parse_endpoint("https://alloy.lan/otlp/").unwrap(),
        Endpoint {
            host: "alloy.lan".into(),
            port: 443,
            path: "/otlp".into()
        }
    );
    assert_eq!(
        parse_endpoint("https://[fd00::1]:4318").unwrap(),
        Endpoint {
            host: "fd00::1".into(),
            port: 4318,
            path: String::new()
        }
    );
    for bad in [
        "http://alloy.lan:4318",
        "alloy.lan:4318",
        "https://",
        "https://h:0",
        "https://h:99999",
        "https://h?x=1",
        "https://h#f",
        "https://h:p",
        "https://h/a b",
    ] {
        assert!(parse_endpoint(bad).is_err(), "{bad} accepted");
    }
    let shown = parse_endpoint("https://user:hunter2@alloy.lan")
        .unwrap_err()
        .to_string();
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(
        !err("[[sink]]\nkind = \"otlp\"\nendpoint = \"https://u:hunter2@h\"\n").contains("hunter2")
    );
}

#[test]
fn component_levels_override_the_default() {
    let cfg = LoggingConfig::parse(
        "[logging]\nlevel = \"warn\"\n[logging.levels]\nengine = \"trace\"\noutbox = \"error\"\n",
    )
    .unwrap();
    assert_eq!(cfg.level_for("engine"), Level::Trace);
    assert_eq!(cfg.level_for("outbox"), Level::Error);
    assert_eq!(cfg.level_for("hub"), Level::Warn);
}
