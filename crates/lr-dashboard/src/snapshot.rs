use lr_core::{JobId, JobState};
use lr_proto::{DashboardJob, DashboardList, DashboardSnapshot, WireSource};
use serde_json::{json, Map, Value};
use std::fmt::Write;

const PAGE: &str = include_str!("page.html");
/// The page stylesheet, served at `/app.css` and inlined into snapshots.
pub(crate) const STYLE: &str = include_str!("page.css");
/// The page code, served at `/app.js` and inlined into snapshots.
pub(crate) const SCRIPT: &str = concat!(include_str!("page_charts.js"), include_str!("page.js"));

fn csp_hash(text: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    let digest = sha2::Sha256::digest(text.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// Line ends as the HTML parser leaves them, so the hash matches the text the browser hashes
/// whatever line ends the checkout has.
fn lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The snapshot's policy: only the inlined page code and stylesheet run, by hash; nothing
/// is fetched.
fn snapshot_csp(style: &str, script: &str) -> String {
    format!(
        "default-src 'none'; style-src {}; script-src {}; img-src data:; connect-src 'none'; \
         base-uri 'none'; form-action 'none'",
        csp_hash(style),
        csp_hash(script)
    )
}

/// Escapes text for HTML element content and quoted attribute values.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn embed_json(value: &Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(c),
        }
    }
    out
}

fn source_name(source: Option<WireSource>) -> &'static str {
    match source {
        Some(WireSource::Session) => "session",
        Some(WireSource::Github) => "github",
        None => "",
    }
}

fn state_name(state: JobState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn cell(out: &mut String, col: &str, text: &str) {
    let _ = write!(out, "<td data-col=\"{col}\">{}</td>", escape_html(text));
}

fn queue_row(out: &mut String, job: &DashboardJob) {
    let d = &job.detail;
    let s = &d.summary;
    let opt = |v: &Option<String>| v.clone().unwrap_or_default();
    let _ = write!(out, "<tr data-job=\"{}\">", escape_html(&s.job.0));
    cell(
        out,
        "position",
        &job.position.map(|p| p.to_string()).unwrap_or_default(),
    );
    cell(out, "job", &s.job.0);
    cell(out, "state", &state_name(s.state));
    cell(out, "project", &opt(&s.project));
    cell(out, "source", source_name(d.source));
    cell(out, "label", &opt(&s.label));
    cell(out, "profile", &opt(&s.profile));
    cell(out, "cwd", &opt(&d.cwd));
    cell(out, "runner", &opt(&s.runner));
    cell(out, "broker", &opt(&job.broker));
    cell(out, "wait", &opt(&d.wait_reason));
    out.push_str("</tr>");
}

/// Table rows of the queue tab: running jobs first, then the queue and the outbox.
fn queue_rows(snapshot: &DashboardSnapshot) -> String {
    let mut out = String::new();
    for list in [
        DashboardList::Running,
        DashboardList::Queued,
        DashboardList::Outbox,
    ] {
        for job in snapshot.jobs.iter().filter(|j| j.list == list) {
            queue_row(&mut out, job);
        }
    }
    out
}

#[derive(Clone, Copy)]
pub(crate) enum Mode<'a> {
    Live { csrf: &'a str },
    Snapshot { logs: &'a [(JobId, String)] },
}

pub(crate) fn render(snapshot: &DashboardSnapshot, mode: Mode<'_>) -> String {
    let (style, script, csp, csrf, config) = match mode {
        Mode::Live { csrf } => (
            "<link rel=\"icon\" href=\"data:,\"><link rel=\"stylesheet\" href=\"/app.css\">"
                .to_string(),
            "<script src=\"/app.js\" defer></script>".to_string(),
            String::new(),
            csrf.to_string(),
            json!({"live": true, "logs": {}}),
        ),
        Mode::Snapshot { logs } => {
            let logs: Map<String, Value> = logs
                .iter()
                .map(|(job, text)| (job.0.clone(), Value::String(text.clone())))
                .collect();
            let (style, script) = (lf(STYLE), lf(SCRIPT));
            (
                format!("<style>{style}</style>"),
                format!("<script>{script}</script>"),
                format!(
                    "<meta http-equiv=\"Content-Security-Policy\" content=\"{}\">",
                    snapshot_csp(&style, &script)
                ),
                String::new(),
                json!({"live": false, "with_logs": !logs.is_empty(), "logs": logs}),
            )
        }
    };
    let without_logs = matches!(mode, Mode::Snapshot { logs } if logs.is_empty());
    let data = if without_logs {
        let mut bare = snapshot.clone();
        for job in &mut bare.jobs {
            job.last_lines.clear();
        }
        serde_json::to_value(&bare)
    } else {
        serde_json::to_value(snapshot)
    }
    .unwrap_or(Value::Null);
    fill(
        PAGE,
        &[
            ("CSP", csp),
            ("CSRF", escape_html(&csrf)),
            ("STYLE", style),
            ("TAKEN", escape_html(&snapshot.taken_at.to_string())),
            ("QUEUE_ROWS", queue_rows(snapshot)),
            ("CONFIG", embed_json(&config)),
            ("DATA", embed_json(&data)),
            ("SCRIPT", script),
        ],
    )
}

/// Substitutes `{{KEY}}` placeholders of the template in one pass, so substituted text is
/// never scanned for placeholders again.
fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let found = after.find("}}").and_then(|end| {
            let key = &after[..end];
            values
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (v, end))
        });
        match found {
            Some((value, end)) => {
                out.push_str(value);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The dashboard as one self-contained HTML file (`status --html`, `Save snapshot`): styles,
/// code and data are inline, nothing is fetched, live updates and actions are off. Logs are
/// embedded only when `logs` is not empty, since they may hold secrets from profile commands.
pub fn render_snapshot(snapshot: &DashboardSnapshot, logs: &[(JobId, String)]) -> String {
    render(snapshot, Mode::Snapshot { logs })
}
