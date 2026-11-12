mod common;

use lr_core::{JobState, StepState, Timestamp};
use lr_proto::{DashboardList, StepView};
use serde_json::{json, Value};

const PAGE: &str = include_str!("../src/page.js");
const CHARTS: &str = include_str!("../src/page_charts.js");

fn function(name: &str) -> &'static str {
    let start = PAGE
        .find(&format!("  function {name}("))
        .unwrap_or_else(|| panic!("{name} in page.js"));
    let end = start + PAGE[start..].find("\n  }\n").unwrap() + 4;
    &PAGE[start..end]
}

const FAKE_DOM: &str = "var document = { createElementNS: function (ns, tag) { return { tag: tag, \
    attrs: {}, kids: [], textContent: '', setAttribute: function (k, v) { this.attrs[k] = v; }, \
    appendChild: function (c) { this.kids.push(c); } }; } };\n";

fn node(program: &str, test: &str) -> Option<Value> {
    let out = match std::process::Command::new("node")
        .args(["-e", program])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            let why = format!("node is not available ({e}), page code not executed");
            let required = std::env::var("LOCALRUNNER_REQUIRE_TEST_TOOLS").unwrap_or_default();
            assert!(
                !required
                    .split(',')
                    .any(|t| t.trim() == "node" || t.trim() == "all"),
                "LOCALRUNNER_REQUIRE_TEST_TOOLS={required} requires node, but {why}"
            );
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "SKIPPED {test}: {why}");
            return None;
        }
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(serde_json::from_slice(&out.stdout).unwrap())
}

#[test]
fn table_and_csv_rows_carry_source_directory_and_the_job_timeout() {
    let mut snap = common::snapshot();
    let run = &mut snap.jobs[0];
    run.detail.summary.started_at = Some(Timestamp(1_700_000_000_000));
    run.detail.summary.timeout_s = Some(600);
    run.detail.steps.push(StepView {
        ord: 1,
        name: "test".into(),
        state: StepState::Queued,
        exit_code: None,
        started_at: None,
        finished_at: None,
        timeout_s: 120,
    });
    let jobs = serde_json::to_string(&snap.jobs).unwrap();
    let program = format!(
        "var S = {{}}; var config = {{ live: false }};\n\
         function now() {{ return 1700000060000; }}\n\
         function dur(ms) {{ return ms == null ? '' : Math.round(ms / 1000) + 's'; }}\n\
         {}\n{}\n\
         var rows = {jobs}.map(row);\n\
         console.log(JSON.stringify(rows.map(function (r) {{ return {{ source: r.source, cwd: r.cwd, \
         wait: r.wait, csv: [r.source, r.cwd].map(csvCell).join(',') }}; }})));",
        function("row"),
        PAGE.lines()
            .find(|l| l.contains("function csvCell("))
            .unwrap(),
    );
    let Some(rows) = node(&program, "table_and_csv_rows") else {
        return;
    };
    assert_eq!(rows[0]["source"], "session");
    assert_eq!(rows[0]["cwd"], "D:/work/alpha");
    assert_eq!(
        rows[0]["wait"], "60s / 600s",
        "elapsed against the job timeout, not the sum of step timeouts"
    );
    assert_eq!(rows[1]["source"], "github");
    assert_eq!(rows[1]["csv"], "github,D:/work/alpha");
}

#[test]
fn the_log_of_the_chosen_step_is_its_section() {
    let log = "=== started on runner r1 at 5 ===\n\
               === step build | project=o/r ===\nb1\nb2\n\
               === step build passed, exit 0, 12 ms ===\n\
               === step test ===\nt1\n=== step test cancelled at 9 ===\n";
    let program = format!(
        "{}\nvar log = {};\nconsole.log(JSON.stringify([stepLog(log, 'build'), stepLog(log, 'test'), \
         stepLog(log, ''), stepLog(log, 'missing')]));",
        function("stepLog"),
        serde_json::to_string(log).unwrap()
    );
    let Some(parts) = node(&program, "the_log_of_the_chosen_step") else {
        return;
    };
    assert_eq!(parts[0], "=== step build | project=o/r ===\nb1\nb2");
    assert_eq!(
        parts[1],
        "=== step test ===\nt1\n=== step test cancelled at 9 ===\n"
    );
    assert_eq!(parts[2], log, "no step chosen shows the whole log");
    assert_eq!(
        parts[3], log,
        "a step without a section shows the whole log"
    );
}

fn texts(node: &Value, out: &mut Vec<String>) {
    if node["tag"] == "text" || node["tag"] == "title" {
        out.push(node["textContent"].as_str().unwrap_or_default().to_string());
    }
    for kid in node["kids"].as_array().into_iter().flatten() {
        texts(kid, out);
    }
}

#[test]
fn stats_show_saturation_by_wait_reason_and_runner_lanes_of_the_day() {
    let mut snap = common::snapshot();
    let now = 1_700_000_100_000u64;
    snap.jobs[1].wait_code = Some("memory".into());
    snap.jobs[2].wait_code = Some("memory".into());
    let mut done = snap.jobs[0].clone();
    done.list = DashboardList::History;
    done.detail.summary.job.0 = "j-done".into();
    done.detail.summary.state = JobState::Passed;
    done.detail.summary.runner = Some("box-2".into());
    done.detail.summary.started_at = Some(Timestamp(now - 3_600_000));
    done.detail.summary.finished_at = Some(Timestamp(now - 1_800_000));
    snap.jobs[0].detail.summary.started_at = Some(Timestamp(now - 60_000));
    snap.jobs.push(done);
    let jobs = serde_json::to_string(&snap.jobs).unwrap();
    let program = format!(
        "{FAKE_DOM}{CHARTS}\n\
         var sat = LRCharts.saturation({jobs}, {now});\n\
         var lanes = LRCharts.lanes({jobs}, {now}, 86400000);\n\
         var bars = LRCharts.bars('Pool time by client', [{{ k: 'a-very-long-client-label-that-does-not-fit', v: 1, cls: 'm', t: '1s' }}]);\n\
         var meter = LRCharts.meter(12288, 16384, 'MB');\n\
         console.log(JSON.stringify({{ sat: sat, lanes: lanes, bars: bars, meter: meter }}));"
    );
    let Some(out) = node(&program, "stats_show_saturation") else {
        return;
    };
    assert_eq!(
        out["sat"],
        json!([{"code": "memory", "name": "memory", "jobs": 2, "ms": 2 * 100_000}])
    );
    let mut lane_text = Vec::new();
    texts(&out["lanes"], &mut lane_text);
    for want in ["box-1", "box-2", "waiting", "now", "-24h"] {
        assert!(
            lane_text.iter().any(|t| t == want),
            "{want} in {lane_text:?}"
        );
    }
    assert!(
        lane_text
            .iter()
            .any(|t| t.starts_with("j-done · alex@laptop")),
        "a lane bar names the job and the client: {lane_text:?}"
    );
    let mut bar_text = Vec::new();
    texts(&out["bars"], &mut bar_text);
    assert!(bar_text.contains(&"a-very-long-client-label-that-does-not-fit".to_string()));
    assert!(bar_text
        .iter()
        .any(|t| t.ends_with('…') && t.chars().count() == 24));
    let meter_width: u64 = out["meter"]["attrs"]["width"].as_u64().unwrap();
    assert!(
        meter_width >= 86 + 7 * "12288/16384MB".len() as u64,
        "the meter label fits: {meter_width}"
    );
}

#[test]
fn the_page_reads_the_new_fields_and_follows_the_hash() {
    let row = function("row");
    assert!(row.contains("s.timeout_s ? s.timeout_s * 1000"), "{row}");
    let stats = function("renderStats");
    assert!(stats.contains("c.label || c.client"), "{stats}");
    assert!(stats.contains("LRCharts.saturation(") && stats.contains("LRCharts.lanes("));
    let runners = function("renderRunners");
    assert!(runners.contains("\"Queue\""), "{runners}");
    assert!(PAGE.contains("window.addEventListener(\"hashchange\""));
    let card = function("fillCard");
    for field in ["d.source", "d.note", "d.branch", "d.cwd", "d.cancelled_by"] {
        assert!(card.contains(field), "{field} on the card");
    }
    assert!(
        card.contains("S.step = S.step === st.name"),
        "a step is chosen on the card"
    );
    assert!(function("showLog").contains("stepLog(S.log, S.step)"));
}
