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
            eprintln!("node is not available, csvCell not executed: {e}");
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
    assert_eq!(
        detail["secrets"],
        serde_json::json!(["registry_auth", "signing"]),
        "the model has names only: there is no field a value could travel in"
    );
}
