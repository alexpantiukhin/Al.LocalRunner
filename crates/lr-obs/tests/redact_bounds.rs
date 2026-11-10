use lr_abstractions::{Level, Record, Role};
use lr_core::Timestamp;
use lr_obs::redact::{Redactor, MAX_FIELD_BYTES, MAX_MESSAGE_BYTES, TRUNCATED};
use lr_obs::MASK;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn record(message: &str) -> Record {
    Record::new(
        Timestamp(1_700_000_000_000),
        Level::Info,
        Role::Runner,
        "agent",
        message,
    )
}

fn plain() -> Redactor {
    Redactor::new(&[])
}

fn straddling(filler: &str, secret: &str, boundary: usize, into: usize) -> String {
    let mut text = String::new();
    while text.len() + filler.len() < boundary - into {
        text.push_str(filler);
        text.push(' ');
    }
    while text.len() < boundary - into {
        text.push('.');
    }
    text.push(' ');
    text.push_str(secret);
    text.push_str(" tail");
    text
}

const URL: &str = "https://admin:hunter2pass@db.lan/x";
const BASIC: &str = "Basic dXNlcjpodW50ZXIycGFzc3dvcmQ=";

#[test]
fn url_credentials_split_by_the_message_cut_do_not_leak() {
    let r = plain();
    for into in [5, 14, 20] {
        let m = straddling("word", URL, MAX_MESSAGE_BYTES, into);
        let out = r.redact(&record(&m));
        let msg = &out.record().message;
        assert!(
            !msg.contains("admin"),
            "into {into}: {}",
            &msg[msg.len() - 40..]
        );
        assert!(!msg.contains("hunter"), "into {into}");
        assert!(msg.ends_with(TRUNCATED));
    }
}

#[test]
fn basic_credentials_split_by_the_field_cut_do_not_leak() {
    let r = plain();
    for into in [12, 17, 25] {
        let v = straddling("word", BASIC, MAX_FIELD_BYTES, into);
        let out = r.redact(&record("m").field("note", &v));
        let value = &out.record().fields[0].1;
        assert!(
            !value.contains("dXNl"),
            "into {into}: {}",
            &value[value.len() - 40..]
        );
        assert!(value.ends_with(TRUNCATED));
    }
}

#[test]
fn field_names_cut_at_their_limit_do_not_leak_credentials() {
    let r = plain();
    let name = straddling("n", URL, 128, 16);
    let out = r.redact(&record("m").field(&name, "v"));
    assert!(!out.record().fields[0].0.contains("admin"));
}

#[test]
fn registered_masks_that_shrink_the_text_do_not_expose_a_cut_credential() {
    let r = plain();
    let secret = "k".repeat(1500);
    r.protect(&secret);
    let mut m = String::new();
    for _ in 0..8 {
        m.push_str(&secret);
        m.push(' ');
    }
    let m = format!(
        "{m}{}",
        straddling("word", URL, MAX_MESSAGE_BYTES + 1500 - m.len(), 14)
    );
    let out = r.redact(&record(&m));
    let msg = &out.record().message;
    assert!(
        !msg.contains("admin"),
        "{}",
        &msg[msg.len().saturating_sub(60)..]
    );
    assert!(!msg.contains("kkkk"));
    assert!(msg.ends_with(TRUNCATED));
}

#[test]
fn multibyte_text_at_the_cut_stays_valid_and_does_not_leak() {
    let r = plain();
    for into in [3, 9, 15] {
        let m = straddling("жёлтый", URL, MAX_MESSAGE_BYTES, into);
        let out = r.redact(&record(&m));
        let msg = &out.record().message;
        assert!(!msg.contains("admin"), "into {into}");
        assert!(msg.len() <= MAX_MESSAGE_BYTES + TRUNCATED.len());
    }
    let wide = "é".repeat(MAX_MESSAGE_BYTES);
    assert!(r
        .redact(&record(&wide))
        .record()
        .message
        .ends_with(TRUNCATED));
}

#[test]
fn exceptions_never_weaken_builtin_or_configured_names() {
    let r = plain();
    assert!(r.is_sensitive_field("token_count"));
    let configured = Redactor::new(&["authority".to_string(), "author".to_string()]);
    assert!(configured.is_sensitive_field("authority"));
    assert!(configured.is_sensitive_field("author"));
    for name in ["tokens_used", "author", "authority", "keyboard", "monkey"] {
        assert!(!r.is_sensitive_field(name), "{name}");
    }
}

#[test]
fn colon_values_are_masked_past_separators() {
    let r = plain();
    assert_eq!(
        r.redact_text("password: ab,cd;ef&gh"),
        format!("password: {MASK}")
    );
}

#[test]
fn double_escaped_json_is_masked() {
    let r = plain();
    assert_eq!(
        r.redact_text(r#"{\\\"token\\\":\\\"abc def\\\"}"#),
        format!(r#"{{\\\"token\\\":\\\"{MASK}\\\"}}"#)
    );
}

#[test]
fn degenerate_secret_sets_stay_cheap() {
    let r = plain();
    for n in 4..516 {
        r.protect(&"a".repeat(n));
    }
    let text = "a".repeat(MAX_MESSAGE_BYTES);
    let started = Instant::now();
    let out = r.redact_text(&text);
    let elapsed = started.elapsed();
    assert!(!out.contains('a'), "{}", &out[..40.min(out.len())]);
    assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
}

#[test]
fn a_secret_registered_before_a_redaction_is_always_masked() {
    let r = Arc::new(plain());
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let r = r.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    r.redact_text("warm up");
                }
            })
        })
        .collect();
    for n in 0..300 {
        let secret = format!("burst-secret-{n:04}");
        r.protect(&secret);
        assert_eq!(
            r.redact_text(&format!("x {secret} y")),
            format!("x {MASK} y")
        );
    }
    for t in readers {
        t.join().unwrap();
    }
}
