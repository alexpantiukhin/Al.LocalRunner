use lr_abstractions::fakes::MemorySecrets;
use lr_abstractions::{Level, Record, Role, SecretSource};
use lr_core::Timestamp;
use lr_obs::redact::{
    RedactingSecrets, Redactor, BUILTIN_FIELDS, MAX_FIELDS, MAX_FIELD_BYTES, MAX_MESSAGE_BYTES,
    MAX_SECRETS, MIN_SECRET_LEN, TRUNCATED,
};
use lr_obs::MASK;
use std::sync::Arc;

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

fn value_of<'a>(r: &'a Record, name: &str) -> &'a str {
    &r.fields.iter().find(|(n, _)| n == name).unwrap().1
}

#[test]
fn secret_values_redacted() {
    let redactor = plain();
    redactor.protect("s3cr3t-value");
    let mut input = record("using s3cr3t-value now").field("note", "x=s3cr3t-value;");
    input.component = "agent s3cr3t-value".into();
    input.machine_id = "m-s3cr3t-value".into();
    let out = redactor.redact(&input);
    let r = out.record();
    assert_eq!(r.message, format!("using {MASK} now"));
    assert_eq!(value_of(r, "note"), format!("x={MASK};"));
    assert_eq!(r.component, format!("agent {MASK}"));
    assert_eq!(r.machine_id, format!("m-{MASK}"));
}

#[test]
fn secret_shorter_than_minimum_is_not_registered() {
    assert_eq!(MIN_SECRET_LEN, 4);
    let redactor = plain();
    redactor.protect("abc");
    redactor.protect("wxyz");
    assert_eq!(redactor.redact_text("abc wxyz"), format!("abc {MASK}"));
}

#[test]
fn longer_overlapping_secret_masked_whole() {
    let redactor = plain();
    redactor.protect("abcd");
    redactor.protect("abcdefgh");
    assert_eq!(redactor.redact_text("[abcdefgh]"), format!("[{MASK}]"));
}

#[test]
fn secret_registry_is_bounded_and_evicts_oldest() {
    assert_eq!(MAX_SECRETS, 4096);
    let redactor = plain();
    for n in 0..=MAX_SECRETS {
        redactor.protect(&format!("value-{n:05}"));
    }
    assert_eq!(redactor.secrets(), MAX_SECRETS);
    assert_eq!(redactor.redact_text("value-00000"), "value-00000");
    assert_eq!(redactor.redact_text("value-04096"), MASK);
    redactor.protect("value-04096");
    assert_eq!(redactor.redact_text("value-00001"), MASK);
}

#[test]
fn builtin_sensitive_field_list_is_exact() {
    assert_eq!(
        BUILTIN_FIELDS,
        &[
            "token",
            "key",
            "password",
            "passwd",
            "secret",
            "authorization",
            "auth",
            "cookie",
            "credential",
            "credentials",
            "command",
            "env",
            "environment",
            "join_code",
        ]
    );
}

#[test]
fn builtin_sensitive_fields_are_masked_whole() {
    let redactor = plain();
    for name in BUILTIN_FIELDS {
        let out = redactor.redact(&record("m").field(name, "visible value"));
        assert_eq!(value_of(out.record(), name), MASK, "field {name}");
    }
}

#[test]
fn configured_fields_extend_builtin_list() {
    let redactor = Redactor::new(&["ssn".to_string()]);
    let out = redactor.redact(&record("m").field("ssn", "1").field("token", "2"));
    assert_eq!(value_of(out.record(), "ssn"), MASK);
    assert_eq!(value_of(out.record(), "token"), MASK);
    redactor.set_fields(&[]);
    assert!(!redactor.is_sensitive_field("ssn"));
    assert!(redactor.is_sensitive_field("token"));
}

#[test]
fn field_names_match_by_segment_case_insensitively() {
    let redactor = plain();
    for name in [
        "api_token",
        "X-Api-Key",
        "privateKey",
        "AUTHORIZATION",
        "db.password",
        "runner_join_code",
        "private_key",
    ] {
        assert!(redactor.is_sensitive_field(name), "{name}");
    }
    for name in [
        "exit_code",
        "monkey",
        "tokens_used",
        "job_id",
        "host",
        "code",
    ] {
        assert!(!redactor.is_sensitive_field(name), "{name}");
    }
}

#[test]
fn key_value_pairs_in_text_are_masked() {
    let r = plain();
    assert_eq!(r.redact_text("token=abc def"), format!("token={MASK} def"));
    assert_eq!(
        r.redact_text("bad password: hunter2, retry"),
        format!("bad password: {MASK}, retry")
    );
    assert_eq!(
        r.redact_text(r#"{"secret":"x y","ok":"1"}"#),
        format!(r#"{{"secret":"{MASK}","ok":"1"}}"#)
    );
    assert_eq!(
        r.redact_text("GET /x?api_key=zz&page=1"),
        format!("GET /x?api_key={MASK}&page=1")
    );
    assert_eq!(r.redact_text("the token expired"), "the token expired");
}

#[test]
fn nested_values_of_sensitive_keys_are_masked_whole() {
    let r = plain();
    assert_eq!(
        r.redact_text(r#"spec "env": {"A":"1","B":{"C":"2"}} tail"#),
        format!(r#"spec "env": {MASK} tail"#)
    );
    assert_eq!(
        r.redact_text("command=[\"sh\", \"-c\"] next"),
        format!("command={MASK} next")
    );
}

#[test]
fn authorization_header_and_bearer_tokens_are_masked() {
    let r = plain();
    let out = r.redact_text("Authorization: Bearer abc.def-ghi");
    assert!(!out.contains("abc.def-ghi"), "{out}");
    assert_eq!(
        r.redact_text("sent bearer XYZ123 ok"),
        format!("sent bearer {MASK} ok")
    );
    assert_eq!(r.redact_text("Basic dXNlcjpwdw=="), format!("Basic {MASK}"));
}

#[test]
fn join_codes_are_masked() {
    let r = plain();
    assert_eq!(
        r.redact_text("use lrj1.AAAA.bb-_.CC9 to join"),
        format!("use {MASK} to join")
    );
    assert_eq!(r.redact_text("xlrj1.abc"), "xlrj1.abc");
}

#[test]
fn private_key_blocks_are_masked() {
    let r = plain();
    let pem = "k: -----BEGIN PRIVATE KEY-----\nMIIabc\n-----END PRIVATE KEY----- done";
    assert_eq!(r.redact_text(pem), format!("k: {MASK} done"));
    let open = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl";
    assert_eq!(r.redact_text(open), MASK);
    let cert = "-----BEGIN CERTIFICATE-----\nMIIx\n-----END CERTIFICATE-----";
    assert_eq!(r.redact_text(cert), cert);
}

#[test]
fn url_credentials_are_masked() {
    let r = plain();
    assert_eq!(
        r.redact_text("fetch https://user:pw@host/x failed"),
        format!("fetch https://{MASK}@host/x failed")
    );
    assert_eq!(r.redact_text("https://host/x@y"), "https://host/x@y");
}

#[test]
fn limits_are_exact() {
    assert_eq!(MAX_MESSAGE_BYTES, 16 * 1024);
    assert_eq!(MAX_FIELD_BYTES, 4 * 1024);
    assert_eq!(MAX_FIELDS, 64);
}

#[test]
fn long_values_truncated_after_redaction() {
    let redactor = plain();
    let secret = "zz-secret-zz";
    redactor.protect(secret);
    let prefix = "a".repeat(MAX_MESSAGE_BYTES - 4);
    let edge = redactor.redact(&record(&format!("{prefix}{secret}")));
    assert_eq!(edge.record().message, format!("{prefix}{MASK}"));
    let message = "b".repeat(MAX_MESSAGE_BYTES + 1);
    let mut input = record(&message).field("note", &"é".repeat(MAX_FIELD_BYTES));
    for n in 0..MAX_FIELDS + 5 {
        input = input.field(&format!("f{n}"), "v");
    }
    let out = redactor.redact(&input);
    let r = out.record();
    assert_eq!(
        r.message,
        format!("{}{TRUNCATED}", "b".repeat(MAX_MESSAGE_BYTES))
    );
    let note = value_of(r, "note");
    assert!(note.ends_with(TRUNCATED));
    assert!(note.len() <= MAX_FIELD_BYTES + TRUNCATED.len());
    assert_eq!(r.fields.len(), MAX_FIELDS);
}

#[test]
fn short_values_are_not_truncated() {
    let redactor = plain();
    let message = "a".repeat(MAX_MESSAGE_BYTES);
    let out = redactor.redact(&record(&message));
    assert_eq!(out.record().message, message);
}

#[tokio::test]
async fn redacting_secrets_registers_every_fetched_value() {
    let store = MemorySecrets::new();
    store.insert("registry_auth", "tok-12345");
    let redactor = Arc::new(plain());
    let secrets = RedactingSecrets::new(store, redactor.clone());
    let value = secrets.get("registry_auth").await.unwrap();
    assert_eq!(value.expose(), "tok-12345");
    assert_eq!(redactor.redact_text("got tok-12345"), format!("got {MASK}"));
    assert!(secrets.get("missing").await.is_err());
}
