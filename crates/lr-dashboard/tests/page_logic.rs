mod common;

use lr_core::{JobState, StepState, Timestamp};
use lr_proto::{DashboardList, StepView};
use serde_json::{json, Value};

const PAGE: &str = include_str!("../src/page.js");
const CHARTS: &str = include_str!("../src/page_charts.js");

fn function(name: &str) -> String {
    common::node::function(PAGE, name)
}

const FAKE_DOM: &str = "var document = { createElementNS: function (ns, tag) { return { tag: tag, \
    attrs: {}, kids: [], textContent: '', setAttribute: function (k, v) { this.attrs[k] = v; }, \
    appendChild: function (c) { this.kids.push(c); } }; } };\n";

fn node(program: &str, test: &str) -> Option<Value> {
    common::node::run(program, test)
}

#[test]
fn the_functions_the_tests_run_are_found_whatever_the_line_ends() {
    let crlf = PAGE.replace("\r\n", "\n").replace('\n', "\r\n");
    for name in ["row", "csvCell", "stepLog", "now", "showLog", "fillCard"] {
        let lf = function(name);
        assert!(
            lf.starts_with(&format!("function {name}(")) && lf.ends_with('}'),
            "{lf}"
        );
        assert_eq!(common::node::function(&crlf, name), lf, "{name}");
    }
    let last = common::node::function(
        "(function () {\n  function tail() { return { a: 1 }; }\n}());",
        "tail",
    );
    assert_eq!(last, "function tail() { return { a: 1 }; }");
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
        function("csvCell"),
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
    assert_eq!(
        parts[0],
        "=== step build | project=o/r ===\nb1\nb2\n=== step build passed, exit 0, 12 ms ==="
    );
    assert_eq!(
        parts[1],
        "=== step test ===\nt1\n=== step test cancelled at 9 ===\n"
    );
    assert_eq!(parts[2], log, "no step chosen shows the whole log");
    assert_eq!(
        parts[3],
        Value::Null,
        "a step without a section is told apart from a section"
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

fn waiting_widths(node: &Value, out: &mut Vec<u64>) {
    let titled = node["kids"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|k| k["tag"] == "title" && k["textContent"] == "orders waiting");
    if node["tag"] == "rect" && titled {
        out.push(node["attrs"]["width"].as_u64().unwrap());
    }
    for kid in node["kids"].as_array().into_iter().flatten() {
        waiting_widths(kid, out);
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
    let mut gone = snap.jobs[1].clone();
    gone.list = DashboardList::History;
    gone.detail.summary.job.0 = "j-gone".into();
    gone.detail.summary.state = JobState::Cancelled;
    gone.detail.summary.queued_at = Timestamp(now - 36_000_000);
    gone.detail.summary.finished_at = Some(Timestamp(now - 35_940_000));
    snap.jobs.push(gone);
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
    let mut widths = Vec::new();
    waiting_widths(&out["lanes"], &mut widths);
    assert!(!widths.is_empty());
    assert!(
        widths.iter().all(|w| *w <= 2),
        "a job cancelled in the queue waited one minute, not until now: {widths:?}"
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
