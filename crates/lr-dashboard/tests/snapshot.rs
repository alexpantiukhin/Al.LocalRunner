mod common;

use base64::Engine;
use lr_core::JobId;
use lr_dashboard::render_snapshot;
use sha2::Digest;

fn assert_self_contained(html: &str) {
    assert!(html.starts_with("<!doctype html>"), "{}", &html[..40]);
    let lower = html.to_ascii_lowercase();
    for external in [" src=", "<link", "<iframe", "<object", "<embed", "<img"] {
        assert!(!lower.contains(external), "external resource `{external}`");
    }
    assert_eq!(lower.matches("<style>").count(), 1);
    let style = &lower[lower.find("<style>").unwrap()..lower.find("</style>").unwrap()];
    for external in ["@import", "url("] {
        assert!(
            !style.contains(external),
            "external style resource `{external}`"
        );
    }
    assert!(lower.contains("<script>"), "page code must be inline");
    assert!(
        lower.contains("connect-src 'none'"),
        "no network from the file"
    );
    assert!(
        !lower.contains("unsafe-inline"),
        "inline code allowed by hash only"
    );
    let csp_start = html.find("Content-Security-Policy\" content=\"").unwrap();
    let csp = &html[csp_start..csp_start + html[csp_start..].find("\">").unwrap()];
    for (open, close) in [("<style>", "</style>"), ("<script>", "</script>")] {
        let start = html.find(open).unwrap() + open.len();
        let end = start + html[start..].find(close).unwrap();
        let inline = &html[start..end];
        assert!(!inline.contains('\r'), "parser-normalized line ends");
        let digest = sha2::Sha256::digest(inline.as_bytes());
        let hash = base64::engine::general_purpose::STANDARD.encode(digest);
        assert!(
            csp.contains(&format!("'sha256-{hash}'")),
            "{open} hash in {csp}"
        );
    }
}

fn csv_cell_source() -> &'static str {
    let script = include_str!("../src/page.js");
    let start = script.find("function csvCell(").unwrap();
    let end = start + script[start..].find('\n').unwrap();
    &script[start..end]
}

#[test]
fn csv_export_defuses_formulas() {
    let cases = [
        "=HYPERLINK(\"http://x\")",
        "+1",
        "-2",
        "@SUM(A1)",
        "\tx",
        "\rx",
        "a,b",
        "plain",
    ];
    let program = format!(
        "{}\nconsole.log(JSON.stringify({}.map(csvCell)));",
        csv_cell_source(),
        serde_json::to_string(&cases).unwrap()
    );
    let out = match std::process::Command::new("node")
        .args(["-e", &program])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            let why = format!("node is not available ({e}), csvCell not executed");
            let required = std::env::var("LOCALRUNNER_REQUIRE_TEST_TOOLS").unwrap_or_default();
            assert!(
                !required
                    .split(',')
                    .any(|t| t.trim() == "node" || t.trim() == "all"),
                "LOCALRUNNER_REQUIRE_TEST_TOOLS={required} requires node, but {why}"
            );
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED csv_export_defuses_formulas: {why}"
            );
            return;
        }
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cells: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        cells,
        [
            "\"'=HYPERLINK(\"\"http://x\"\")\"",
            "'+1",
            "'-2",
            "'@SUM(A1)",
            "'\tx",
            "\"'\rx\"",
            "\"a,b\"",
            "plain",
        ]
    );
}

#[test]
fn snapshot_is_single_file() {
    let snapshot = common::snapshot();
    let html = render_snapshot(&snapshot, &[]);
    assert_self_contained(&html);

    assert!(html.contains("data-job=\"j-queued\""));
    assert!(html.contains("<td data-col=\"project\">alpha</td>"));
    assert!(html.contains("\"live\":false"));
    assert!(
        html.contains("var LRCharts = (function"),
        "the chart code is inlined under the same hash"
    );
    assert!(html.contains("content=\"\""), "no CSRF token in a snapshot");
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("\\u003cscript\\u003ealert(1)"));
    assert!(
        !html.contains("tail secret line"),
        "last lines are log text, kept out without logs"
    );
    assert!(!html.contains("secret line"));

    let logs = vec![(
        JobId("j-run".into()),
        "secret line\n</script><b>".to_string(),
    )];
    let with_logs = render_snapshot(&snapshot, &logs);
    assert_self_contained(&with_logs);
    assert!(with_logs.contains("secret line"));
    assert!(with_logs.contains("tail secret line"));
    assert!(!with_logs.contains("</script><b>"));
}

#[test]
fn job_card_shows_secret_names_without_values() {
    let mut snapshot = common::snapshot();
    snapshot.jobs[0].detail.secrets = vec!["registry_auth".into(), "signing".into()];
    let html = render_snapshot(&snapshot, &[]);
    assert!(
        html.contains("\"secrets\":[\"registry_auth\",\"signing\"]"),
        "the card data carries the names"
    );
    let script = include_str!("../src/page.js");
    let card = &script[script.find("function fillCard(").unwrap()..];
    let card = &card[..card.find("var dl").unwrap()];
    assert!(
        card.contains("[\"Secrets\", (d.secrets || []).join(\", \")]"),
        "the card lists the names under Secrets"
    );
    let detail = serde_json::to_value(&snapshot.jobs[0].detail).unwrap();
    let mut keys: Vec<&str> = detail
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "branch",
            "cancelled_by",
            "commit",
            "cwd",
            "github_run_url",
            "note",
            "os_user",
            "secrets",
            "source",
            "steps",
            "summary",
            "wait_reason"
        ],
        "the card model has no field a secret value could travel in"
    );
    let mut with_value = detail.clone();
    with_value["secrets"] = serde_json::json!([{"name": "registry_auth", "value": "hunter2"}]);
    assert!(
        serde_json::from_value::<lr_proto::JobDetail>(with_value).is_err(),
        "a secret entry is a bare name: an entry with a value does not fit the model"
    );
}

#[test]
fn job_card_shows_the_reason_code_of_the_end() {
    let mut snapshot = common::snapshot();
    snapshot.jobs[0].detail.summary.reason = Some(lr_proto::ReasonCode::SenderGone);
    let html = render_snapshot(&snapshot, &[]);
    assert!(
        html.contains("\"reason\":\"sender_gone\""),
        "the card data carries the code"
    );
    let script = include_str!("../src/page.js");
    let card = &script[script.find("function fillCard(").unwrap()..];
    let card = &card[..card.find("var dl").unwrap()];
    assert!(
        card.contains("s = d.summary"),
        "s is the summary of the card"
    );
    assert!(
        card.contains("[\"Reason\", s.reason]"),
        "the card lists the code under Reason"
    );
}
