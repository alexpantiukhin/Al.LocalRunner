mod common;

use lr_core::JobId;
use lr_dashboard::render_snapshot;

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
    assert!(!html.contains("secret line"));

    let logs = vec![(
        JobId("j-run".into()),
        "secret line\n</script><b>".to_string(),
    )];
    let with_logs = render_snapshot(&snapshot, &logs);
    assert_self_contained(&with_logs);
    assert!(with_logs.contains("secret line"));
    assert!(!with_logs.contains("</script><b>"));
}
