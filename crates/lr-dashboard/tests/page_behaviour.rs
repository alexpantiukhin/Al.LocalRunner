mod common;

use lr_core::{JobState, StepState, Timestamp};
use lr_proto::{DashboardSnapshot, StepView};
use serde_json::Value;

const DOM: &str = include_str!("page_dom.js");
const CHARTS: &str = include_str!("../src/page_charts.js");
const PAGE: &str = include_str!("../src/page.js");

const LOG: &str = "=== started on runner box-1 at 5 ===\n\
                   === step build | project=beta ===\nb1\n\
                   === step test ===\nt1\n";

fn step(ord: u32, name: &str, state: StepState) -> StepView {
    StepView {
        ord,
        name: name.into(),
        state,
        exit_code: None,
        started_at: (state != StepState::Queued).then_some(Timestamp(1_700_000_000_000)),
        finished_at: None,
        timeout_s: 600,
    }
}

fn running(build: StepState, test: StepState, job: JobState) -> String {
    let mut snap: DashboardSnapshot = common::snapshot();
    let run = &mut snap.jobs[0].detail;
    run.summary.state = job;
    run.summary.started_at = Some(Timestamp(1_700_000_000_000));
    run.steps = vec![step(0, "build", build), step(1, "test", test)];
    let mut v = serde_json::to_value(&snap).unwrap();
    v["runners"] = serde_json::json!([{
        "broker": "home", "stale": false,
        "pool": {"used_mem_mb": 0, "total_mem_mb": 16384, "used_cpu": 0, "total_cpu": 8},
        "runners": [{"name": "box-1", "online": true, "last_seen": null,
            "caps": {"platforms": ["windows"], "mem_mb": 16384, "cpu": 8},
            "free_mem_mb": 16384, "free_cpu": 8, "held_tags": [], "services": [], "unmet": []}]
    }]);
    v["stats"]["clients"] =
        serde_json::json!([{"client": "key-7", "label": "", "runs": 2, "busy_ms": 5000}]);
    v.to_string()
}

fn run(scenario: &str, test: &str) -> Option<Value> {
    run_with("{ live: true, logs: {} }", scenario, test)
}

fn run_with(config: &str, scenario: &str, test: &str) -> Option<Value> {
    let first = running(StepState::Running, StepState::Queued, JobState::Running);
    let program = format!(
        "{DOM}\nstart({config}, {first});\n{CHARTS}\n{PAGE}\n\
         var $ = function (id) {{ return document.getElementById(id); }};\n\
         var LOG = {};\n\
         (async function () {{ var out = {{}};\n{scenario}\n\
         console.log(JSON.stringify(out)); }}()).catch(function (e) {{ console.error(e); process.exit(1); }});",
        serde_json::to_string(LOG).unwrap()
    );
    common::node::run(&program, test)
}

const CARD: &str = r#"
  var steps = function () { return $("card-steps").children.map(function (s) { return s.className; }); };
  var seen = function () { return { steps: steps(), title: $("log-title").textContent, log: $("log").textContent }; };
  sources[0].onopen(); await tick();
  logText = LOG;
  $("queue-body").children[0].fire("click"); await tick();
  out.opened = seen();
  $("card-steps").children[1].fire("click");
  out.picked = seen();
  $("card-steps").children[1].fire("click");
  out.unpicked = seen();
"#;

#[test]
fn the_card_shows_the_log_of_the_running_step_and_of_the_step_clicked() {
    let Some(out) = run(CARD, "the_card_shows_the_log_of_the_running_step") else {
        return;
    };
    assert_eq!(out["opened"]["steps"][0], "step chosen", "{out}");
    assert_eq!(out["opened"]["title"], "Log · step build");
    assert_eq!(
        out["opened"]["log"],
        "=== step build | project=beta ===\nb1"
    );
    assert_eq!(out["picked"]["steps"][1], "step chosen");
    assert_eq!(out["picked"]["steps"][0], "step");
    assert_eq!(out["picked"]["title"], "Log · step test");
    assert_eq!(out["picked"]["log"], "=== step test ===\nt1\n");
    assert_eq!(
        out["unpicked"]["title"], "Log",
        "a second click shows the whole log"
    );
    assert_eq!(out["unpicked"]["log"], LOG);
}

const CLOCK: &str = r#"
  var clock = 1700000030000;
  Date.now = function () { return clock; };
  var cell = function () {
    var tds = $("queue-body").children[0].children;
    return tds.filter(function (td) { return td.getAttribute("data-col") === "wait"; })[0].textContent;
  };
  var bar = function () { return $("card-steps").children[0].children[2].textContent; };
  var tickSecond = function () { intervals.filter(function (i) { return i.ms === 1000; }).forEach(function (i) { i.f(); }); };
  sources[0].onopen(); await tick();
  logText = LOG;
  $("queue-body").children[0].fire("click"); await tick();
  out.before = { grid: cell(), card: bar() };
  clock += 5000;
  tickSecond();
  out.after = { grid: cell(), card: bar() };
  $("pause").fire("click");
  clock += 5000;
  tickSecond();
  out.paused = { grid: cell(), card: bar() };
"#;

#[test]
fn the_time_of_a_running_job_ticks_in_the_grid_and_in_the_card_without_a_new_snapshot() {
    let Some(out) = run(CLOCK, "the_time_of_a_running_job_ticks") else {
        return;
    };
    assert_eq!(out["before"]["grid"], "30s / 20m0s", "{out}");
    assert_eq!(out["before"]["card"], "30s / 10m0s");
    assert_eq!(out["after"]["grid"], "35s / 20m0s", "{out}");
    assert_eq!(out["after"]["card"], "35s / 10m0s");
    assert_eq!(out["paused"], out["after"], "a paused page stands still");
}

#[test]
fn the_chosen_step_follows_the_run_and_a_missing_section_is_said() {
    let next = running(StepState::Passed, StepState::Running, JobState::Running);
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let scenario = format!(
        r#"
  sources[0].onopen(); await tick();
  $("log-follow").checked = true;
  logText = "=== step test ===\nt1\n";
  $("queue-body").children[0].fire("click"); await tick();
  out.cut = {{ title: $("log-title").textContent, log: $("log").textContent }};
  logText = LOG;
  nextData = {next}; sources[0].onopen(); await tick();
  out.next = {{ title: $("log-title").textContent, log: $("log").textContent }};
  nextData = {done}; sources[0].onopen(); await tick();
  out.done = {{ title: $("log-title").textContent }};
"#
    );
    let Some(out) = run(&scenario, "the_chosen_step_follows_the_run") else {
        return;
    };
    assert_eq!(
        out["cut"]["title"], "Log (step build is not in the loaded part)",
        "{out}"
    );
    assert_eq!(out["cut"]["log"], "=== step test ===\nt1\n");
    assert_eq!(out["next"]["title"], "Log · step test");
    assert_eq!(out["next"]["log"], "=== step test ===\nt1\n");
    assert_eq!(
        out["done"]["title"], "Log",
        "a finished run shows the whole log"
    );
}

#[test]
fn a_changed_hash_restores_the_view_with_its_tab_and_filters() {
    let scenario = r##"
  location.hash = "#tab=runners";
  handlers.hashchange();
  out.runners = { queue: $("tab-queue").hidden, runners: $("tab-runners").hidden, text: $("tab-runners").text() };
  location.hash = "#tab=stats";
  handlers.hashchange();
  out.stats = $("tab-stats").text();
  location.hash = "#tab=queue&project=alpha";
  handlers.hashchange();
  out.queue = { project: $("f-project").value, rows: $("queue-body").children.map(function (r) { return r.attrs["data-job"]; }) };
"##;
    let Some(out) = run(scenario, "a_changed_hash_restores_the_view") else {
        return;
    };
    assert_eq!(out["runners"]["queue"], true, "{out}");
    assert_eq!(out["runners"]["runners"], false);
    let runners = out["runners"]["text"].as_str().unwrap();
    assert!(runners.contains("home · queue 2"), "{runners}");
    assert!(runners.contains("Queue"), "{runners}");
    let stats = out["stats"].as_str().unwrap();
    assert!(
        stats.contains("key-7"),
        "a client without a label is named by its key: {stats}"
    );
    assert!(stats.contains("Saturation of the queue now"), "{stats}");
    assert_eq!(out["queue"]["project"], "alpha");
    assert_eq!(out["queue"]["rows"], serde_json::json!(["j-queued"]));
}

#[test]
fn a_step_that_never_ran_shows_a_full_skipped_bar_once_the_run_is_over() {
    let over = running(StepState::Failed, StepState::Queued, JobState::Cancelled);
    let scenario = format!(
        r#"
  sources[0].onopen(); await tick();
  $("queue-body").children[0].fire("click"); await tick();
  nextData = {over}; sources[0].onopen(); await tick();
  var row = $("card-steps").children[1], fill = row.children[1].children[0];
  out.label = row.children[0].textContent;
  out.cls = fill.className;
  out.width = fill.style.width;
"#
    );
    let Some(out) = run(&scenario, "a_step_that_never_ran_shows_a_full_skipped_bar") else {
        return;
    };
    assert_eq!(out["label"], "test · skipped", "{out}");
    assert_eq!(out["cls"], "fill skipped");
    assert_eq!(out["width"], "100%");
}

#[test]
fn a_snapshot_without_logs_says_so_once_and_offers_nothing_to_copy() {
    let scenario = r#"
  await tick();
  $("queue-body").children[0].fire("click"); await tick();
  out.title = $("log-title").textContent;
  out.log = $("log").textContent;
  out.copy = $("log-copy").disabled;
  out.download = $("log-download").disabled;
"#;
    let config = "{ live: false, logs: {}, with_logs: false }";
    let Some(out) = run_with(config, scenario, "a_snapshot_without_logs_says_so_once") else {
        return;
    };
    assert_eq!(out["title"], "Log", "{out}");
    assert_eq!(out["log"], "(logs are not included in this snapshot)");
    assert_eq!(out["copy"], true);
    assert_eq!(out["download"], true);
}
