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
    run_snapshot(config, &first, scenario, test)
}

fn run_snapshot(config: &str, first: &str, scenario: &str, test: &str) -> Option<Value> {
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
fn a_runner_is_shown_online_stale_or_offline_by_how_recently_it_was_heard() {
    let mut v: Value = serde_json::from_str(&running(
        StepState::Running,
        StepState::Queued,
        JobState::Running,
    ))
    .unwrap();
    let base = v["runners"][0]["runners"][0].clone();
    let named = |name: &str, online: bool, liveness: Option<&str>, silent: Option<u64>| {
        let mut r = base.clone();
        r["name"] = name.into();
        r["online"] = online.into();
        if let Some(l) = liveness {
            r["liveness"] = l.into();
        }
        if let Some(s) = silent {
            r["silent_for_s"] = s.into();
        }
        r
    };
    v["runners"][0]["runners"] = serde_json::json!([
        named("fresh", true, Some("online"), Some(0)),
        named("quiet", true, Some("stale"), Some(40)),
        named("gone", false, Some("offline"), Some(90)),
        named("legacy-up", true, None, None),
        named("legacy-down", false, None, None),
    ]);
    let scenario = r##"
  location.hash = "#tab=runners";
  handlers.hashchange();
  out.rows = $("tab-runners").text();
"##;
    let Some(out) = run_snapshot(
        "{ live: true, logs: {} }",
        &v.to_string(),
        scenario,
        "a_runner_is_shown_online_stale_or_offline",
    ) else {
        return;
    };
    let text = out["rows"].as_str().unwrap();
    assert_eq!(text.matches("stale (silent 40 s)").count(), 1, "{text}");
    assert_eq!(text.matches("offline").count(), 2, "{text}");
    assert_eq!(text.matches("online").count(), 2, "{text}");
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

#[test]
fn a_refused_order_is_shown_as_rejected_not_as_cancelled() {
    let mut v: Value = serde_json::from_str(&running(
        StepState::Queued,
        StepState::Queued,
        JobState::Cancelled,
    ))
    .unwrap();
    v["jobs"][0]["detail"]["summary"]["reason"] = serde_json::json!("rejected_pool_memory");
    let scenario = r#"
      sources[0].onopen(); await tick();
      var tr = $("queue-body").children[0];
      out.states = tr.children.filter(function (c) { return c.className && c.className.indexOf("state-") === 0; }).map(function (c) { return c.className; });
    "#;
    let Some(out) = run_snapshot(
        "{ live: true, logs: {} }",
        &v.to_string(),
        scenario,
        "rejected",
    ) else {
        return;
    };
    assert_eq!(out["states"][0], "state-rejected", "{out}");
}

#[test]
fn the_history_grid_has_a_finished_column_sorted_by_finish_time_until_a_header_is_clicked() {
    let mut v: Value = serde_json::from_str(&running(
        StepState::Queued,
        StepState::Queued,
        JobState::Passed,
    ))
    .unwrap();
    let mut done = |idx: usize, job: &str, queued: u64, finished: Option<u64>| {
        let mut j = v["jobs"][0].clone();
        j["list"] = serde_json::json!("history");
        j["detail"]["summary"]["job"] = serde_json::json!(job);
        j["detail"]["summary"]["queued_at"] = serde_json::json!(queued);
        j["detail"]["summary"]["finished_at"] = serde_json::json!(finished);
        j["detail"]["summary"]["state"] = serde_json::json!("passed");
        v["jobs"].as_array_mut().unwrap().insert(idx, j);
    };
    done(0, "queued-first", 1_000, Some(9_000));
    done(1, "queued-last", 5_000, Some(6_000));
    done(2, "unfinished", 7_000, None);
    done(3, "middle", 3_000, Some(8_000));
    let scenario = r##"
      sources[0].onopen(); await tick();
      var ids = function () { return $("history-body").children.map(function (r) { return r.attrs["data-job"]; }); };
      location.hash = "#tab=history";
      handlers.hashchange();
      out.default = ids();
      out.hash = location.hash;
      out.cols = $("history-body").children[0].children.map(function (c) { return c.attrs["data-col"]; });
      location.hash = "#tab=history&sort=-queued";
      handlers.hashchange();
      out.byQueued = ids();
      location.hash = "#tab=history&sort=-finished";
      handlers.hashchange();
      out.explicit = ids();
    "##;
    let Some(out) = run_snapshot(
        "{ live: true, logs: {} }",
        &v.to_string(),
        scenario,
        "finished",
    ) else {
        return;
    };
    assert_eq!(
        out["default"],
        serde_json::json!(["queued-first", "middle", "queued-last", "unfinished"]),
        "{out}"
    );
    assert_eq!(out["cols"][12], "finished", "{out}");
    assert_eq!(
        out["byQueued"][0], "unfinished",
        "a header click overrides the default: {out}"
    );
    assert_eq!(out["explicit"][0], "queued-first", "{out}");
}

#[test]
fn the_history_pager_shows_the_position_and_asks_the_server_for_the_page_wanted() {
    let mut v: Value = serde_json::from_str(&running(
        StepState::Queued,
        StepState::Queued,
        JobState::Passed,
    ))
    .unwrap();
    let mut done = v["jobs"][0].clone();
    done["list"] = serde_json::json!("history");
    v["jobs"].as_array_mut().unwrap().push(done);
    v["history_page"] = serde_json::json!({"offset": 0, "limit": 100, "total": 250});
    v["queue_count"] = serde_json::json!({"shown": 1, "total": 7});
    let scenario = r##"
      var urls = [], plainFetch = globalThis.fetch;
      globalThis.fetch = function (u) { urls.push(String(u)); return plainFetch(u); };
      sources[0].onopen(); await tick();
      urls.length = 0;
      out.note = $("queue-note").textContent;
      out.noteHidden = $("queue-note").hidden;
      location.hash = "#tab=history";
      handlers.hashchange();
      out.text = $("hp-text").textContent;
      out.prevDisabled = $("hp-prev").disabled;
      out.nextDisabled = $("hp-next").disabled;
      $("hp-next").fire("click"); await tick();
      out.next = urls.slice();
      $("hp-size").value = "50";
      $("hp-size").fire("change"); await tick();
      out.size = urls.slice(1);
    "##;
    let Some(out) = run_snapshot(
        "{ live: true, logs: {} }",
        &v.to_string(),
        scenario,
        "history_pager",
    ) else {
        return;
    };
    assert_eq!(
        out["note"], "shown 1 of 7 queued orders; the broker's list limit cut the rest",
        "{out}"
    );
    assert_eq!(out["noteHidden"], false, "{out}");
    assert_eq!(out["text"], "shown 1 of 250 runs, page 1 of 3", "{out}");
    assert_eq!(out["prevDisabled"], true, "{out}");
    assert_eq!(out["nextDisabled"], false, "{out}");
    assert_eq!(
        out["next"][0], "/api/snapshot?history_offset=100&history_limit=100",
        "{out}"
    );
    assert_eq!(
        out["size"][0], "/api/snapshot?history_offset=0&history_limit=50",
        "a new page size starts from the first page: {out}"
    );
}

#[test]
fn without_a_total_the_history_cannot_page_and_a_complete_queue_has_no_note() {
    let mut v: Value = serde_json::from_str(&running(
        StepState::Queued,
        StepState::Queued,
        JobState::Passed,
    ))
    .unwrap();
    v["queue_count"] = serde_json::json!({"shown": 3, "total": 3});
    let scenario = r##"
      sources[0].onopen(); await tick();
      out.noteHidden = $("queue-note").hidden;
      location.hash = "#tab=history";
      handlers.hashchange();
      out.prevDisabled = $("hp-prev").disabled;
      out.nextDisabled = $("hp-next").disabled;
    "##;
    let Some(out) = run_snapshot(
        "{ live: true, logs: {} }",
        &v.to_string(),
        scenario,
        "history_without_total",
    ) else {
        return;
    };
    assert_eq!(out["noteHidden"], true, "{out}");
    assert_eq!(out["prevDisabled"], true, "{out}");
    assert_eq!(out["nextDisabled"], true, "{out}");
}

const FOLLOW_SETUP: &str = r#"
  var logFetches = 0;
  var plainFetch = globalThis.fetch;
  globalThis.fetch = function (url) { if (String(url).indexOf("/api/log/") === 0) logFetches += 1; return plainFetch(url); };
  var pre = $("log");
  pre.scrollHeight = 1000; pre.clientHeight = 100;
  var follow = $("log-follow");
  follow.checked = true;
  var tickLog = function () { intervals.filter(function (i) { return i.ms === 2000; }).forEach(function (i) { i.f(); }); };
  sources[0].onopen(); await tick();
  logText = LOG;
  $("queue-body").children[0].fire("click"); await tick();
  var state = function () { return { checked: follow.checked, disabled: !!follow.disabled, top: pre.scrollTop, loads: logFetches }; };
"#;

#[test]
fn a_finished_job_opens_with_follow_off_and_a_scroll_up_survives_the_timer_and_a_refresh() {
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let scenario = format!(
        "{FOLLOW_SETUP}{}",
        r#"
  out.opened = state();
  pre.scrollTop = 120; pre.fire("scroll");
  tickLog(); await tick();
  sources[0].onopen(); await tick();
  out.after = state();
"#
    );
    let Some(out) = run_snapshot(
        "{ live: true, logs: {} }",
        &done,
        &scenario,
        "finished_follow",
    ) else {
        return;
    };
    assert_eq!(out["opened"]["checked"], false, "{out}");
    assert_eq!(out["opened"]["disabled"], true, "{out}");
    assert_eq!(
        out["after"]["top"], 120,
        "the scroll position is kept: {out}"
    );
    assert_eq!(out["after"]["loads"], out["opened"]["loads"], "{out}");
    assert_eq!(out["after"]["checked"], false, "{out}");
}

#[test]
fn a_running_job_follows_the_end_until_scrolled_up_and_ticking_follow_returns_to_it() {
    let scenario = format!(
        "{FOLLOW_SETUP}{}",
        r#"
  out.opened = state();
  logText = LOG + "more\n";
  tickLog(); await tick();
  out.followed = state();
  pre.scrollTop = 100; pre.fire("scroll");
  out.scrolledUp = state();
  tickLog(); await tick();
  out.idle = state();
  follow.checked = true; follow.fire("change");
  out.back = state();
"#
    );
    let Some(out) = run_with("{ live: true, logs: {} }", &scenario, "running_follow") else {
        return;
    };
    assert_eq!(out["opened"]["checked"], true, "{out}");
    assert_eq!(out["opened"]["disabled"], false, "{out}");
    assert_eq!(out["followed"]["top"], 1000, "{out}");
    assert_eq!(out["scrolledUp"]["checked"], false, "{out}");
    assert_eq!(out["scrolledUp"]["top"], 100, "{out}");
    assert_eq!(out["idle"]["top"], 100, "{out}");
    assert_eq!(out["back"]["checked"], true, "{out}");
    assert_eq!(out["back"]["top"], 1000, "{out}");
}

#[test]
fn a_followed_job_that_finishes_gets_one_last_load_and_follow_switches_off() {
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let scenario = FOLLOW_SETUP.to_string()
        + &format!(
            r#"
  pre.scrollTop = 1000;
  var before = state();
  logText = LOG + "last\n";
  nextData = {done}; sources[0].onopen(); await tick(); await tick();
  out.atEnd = state();
  out.atEndLog = pre.textContent;
  tickLog(); await tick();
  sources[0].onopen(); await tick();
  out.atEndLater = state();
  out.before = before;
"#
        );
    let Some(out) = run_with("{ live: true, logs: {} }", &scenario, "finishing_follow") else {
        return;
    };
    assert_eq!(out["atEnd"]["checked"], false, "{out}");
    assert_eq!(out["atEnd"]["disabled"], true, "{out}");
    assert_eq!(out["atEnd"]["top"], 1000, "{out}");
    assert!(
        out["atEndLog"].as_str().unwrap().ends_with(
            "last
"
        ),
        "{out}"
    );
    assert_eq!(out["atEndLater"]["loads"], out["atEnd"]["loads"], "{out}");
}

#[test]
fn a_job_that_finishes_while_scrolled_up_keeps_the_scroll_position() {
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let scenario = FOLLOW_SETUP.to_string()
        + &format!(
            r#"
  pre.scrollTop = 100; pre.fire("scroll");
  logText = LOG + "last\n";
  nextData = {done}; sources[0].onopen(); await tick(); await tick();
  out.state = state();
"#
        );
    let Some(out) = run_with(
        "{ live: true, logs: {} }",
        &scenario,
        "finishing_scrolled_up",
    ) else {
        return;
    };
    assert_eq!(out["state"]["checked"], false, "{out}");
    assert_eq!(out["state"]["disabled"], true, "{out}");
    assert_eq!(out["state"]["top"], 100, "{out}");
}

#[test]
fn the_own_runs_pin_does_not_override_the_history_order_and_a_zero_finish_is_kept() {
    let mut v: Value = serde_json::from_str(&running(
        StepState::Queued,
        StepState::Queued,
        JobState::Passed,
    ))
    .unwrap();
    let mut add = |job: &str, label: &str, finished: Option<u64>| {
        let mut j = v["jobs"][0].clone();
        j["list"] = serde_json::json!("history");
        j["detail"]["summary"]["job"] = serde_json::json!(job);
        j["detail"]["summary"]["label"] = serde_json::json!(label);
        j["detail"]["summary"]["finished_at"] = serde_json::json!(finished);
        j["detail"]["summary"]["state"] = serde_json::json!("passed");
        v["jobs"].as_array_mut().unwrap().push(j);
    };
    add("own-old", "me", Some(1_000));
    add("other-new", "they", Some(9_000));
    add("epoch", "they", Some(0));
    add("open", "they", None);
    let scenario = r##"
      sources[0].onopen(); await tick();
      var ids = function () { return $("history-body").children.map(function (r) { return r.attrs["data-job"]; }); };
      location.hash = "#tab=history";
      handlers.hashchange();
      out.default = ids();
      out.epoch = $("history-body").children[2].children.filter(function (c) { return c.attrs["data-col"] === "finished"; })[0].textContent;
      location.hash = "#tab=history&sort=finished";
      handlers.hashchange();
      out.clicked = ids();
    "##;
    let Some(out) = run_snapshot(
        "(location.search = \"?as=me\", { live: true, logs: {} })",
        &v.to_string(),
        scenario,
        "pin_history",
    ) else {
        return;
    };
    assert_eq!(
        out["default"],
        serde_json::json!(["other-new", "own-old", "epoch", "open"]),
        "{out}"
    );
    assert_ne!(out["epoch"], "", "{out}");
    assert_eq!(
        out["clicked"],
        serde_json::json!(["epoch", "own-old", "other-new", "open"]),
        "{out}"
    );
}

#[test]
fn a_runner_without_limit_support_is_marked_with_or_without_the_enforcement_field() {
    let mut v: Value = serde_json::from_str(&running(
        StepState::Running,
        StepState::Queued,
        JobState::Running,
    ))
    .unwrap();
    let base = v["runners"][0]["runners"][0].clone();
    let named = |name: &str, enforcement: Option<&str>, limit: bool| {
        let mut r = base.clone();
        r["name"] = name.into();
        r["caps"]["enforce_limit"] = limit.into();
        match enforcement {
            Some(e) => r["caps"]["enforcement"] = e.into(),
            None => {
                r["caps"].as_object_mut().unwrap().remove("enforcement");
            }
        }
        r
    };
    v["runners"][0]["runners"] = serde_json::json!([
        named("current", Some("os"), true),
        named("watching", Some("watcher"), false),
        named("legacy", None, false),
    ]);
    let scenario = r##"
      location.hash = "#tab=runners";
      handlers.hashchange();
      out.rows = $("tab-runners").text();
    "##;
    let Some(out) = run_snapshot(
        "{ live: true, logs: {} }",
        &v.to_string(),
        scenario,
        "legacy_enforcement",
    ) else {
        return;
    };
    let text = out["rows"].as_str().unwrap();
    assert_eq!(text.matches("(limit runs as warn)").count(), 2, "{text}");
    assert!(text.contains("unknown (limit runs as warn)"), "{text}");
    assert!(text.contains("watcher (limit runs as warn)"), "{text}");
}

#[test]
fn a_held_tag_without_a_visible_job_has_no_arrow() {
    let mut v: Value = serde_json::from_str(&running(
        StepState::Running,
        StepState::Queued,
        JobState::Running,
    ))
    .unwrap();
    v["runners"][0]["runners"][0]["held_tags"] = serde_json::json!([
        {"tag": "gpu", "job": "", "exclusive": true},
        {"tag": "db", "job": "j7", "exclusive": false}
    ]);
    let scenario = r##"
      location.hash = "#tab=runners";
      handlers.hashchange();
      out.rows = $("tab-runners").text();
    "##;
    let Some(out) = run_snapshot(
        "{ live: true, logs: {} }",
        &v.to_string(),
        scenario,
        "masked_held_tag",
    ) else {
        return;
    };
    let text = out["rows"].as_str().unwrap();
    assert!(text.contains("gpu!, db←j7"), "{text}");
    assert!(!text.contains("gpu!←"), "{text}");
}

const DEFERRED: &str = r#"
  var pending = [];
  var plainFetch = globalThis.fetch;
  globalThis.fetch = function (url) {
    if (String(url).indexOf("/api/log/") !== 0) return plainFetch(url);
    return new Promise(function (resolve) {
      pending.push({ url: String(url), answer: function (text) { resolve({ ok: true, text: function () { return Promise.resolve(text); } }); } });
    });
  };
  var flush = async function () { for (var i = 0; i < 30; i += 1) await Promise.resolve(); };
  var pre = $("log");
  pre.scrollHeight = 1000; pre.clientHeight = 100;
  var follow = $("log-follow");
  follow.checked = true;
  var tickLog = function () { intervals.filter(function (i) { return i.ms === 2000; }).forEach(function (i) { i.f(); }); };
  sources[0].onopen(); await flush();
  $("queue-body").children[0].fire("click"); await flush();
  pending[0].answer(LOG); await flush();
  var settle = pending.length;
"#;

fn deferred(next: &str, body: &str) -> Option<Value> {
    let scenario = format!("{DEFERRED}{body}");
    let first = running(StepState::Running, StepState::Queued, JobState::Running);
    let program_next = format!("var done = {next};\n{scenario}");
    run_snapshot(
        "{ live: true, logs: {} }",
        &first,
        &program_next,
        "deferred_log",
    )
}

#[test]
fn a_slow_request_from_before_the_finish_never_overwrites_the_final_log() {
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let Some(out) = deferred(
        &done,
        r#"
  nextData = done; sources[0].onopen(); await flush();
  out.started = pending.length - settle;
  pending[pending.length - 1].answer(LOG + "final\n"); await flush();
  pending[settle].answer(LOG + "stale\n"); await flush();
  out.text = pre.textContent;
  out.checked = follow.checked;
"#,
    ) else {
        return;
    };
    let text = out["text"].as_str().unwrap();
    assert!(text.ends_with("final\n"), "{out}");
    assert!(!text.contains("stale"), "{out}");
    assert_eq!(out["checked"], false, "{out}");
}

#[test]
fn a_scroll_up_while_the_final_load_is_pending_survives_its_response() {
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let Some(out) = deferred(
        &done,
        r#"
  pre.scrollTop = 1000;
  nextData = done; sources[0].onopen(); await flush();
  out.disabled = !!follow.disabled;
  out.checkedAtFinish = follow.checked;
  pre.scrollTop = 100; pre.fire("scroll");
  pending[pending.length - 1].answer(LOG + "final\n"); await flush();
  out.top = pre.scrollTop;
  out.text = pre.textContent;
"#,
    ) else {
        return;
    };
    assert_eq!(out["disabled"], true, "{out}");
    assert_eq!(out["checkedAtFinish"], false, "{out}");
    assert_eq!(out["top"], 100, "{out}");
    assert!(out["text"].as_str().unwrap().ends_with("final\n"), "{out}");
}

#[test]
fn a_followed_job_that_finishes_shows_the_end_of_its_final_log() {
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let Some(out) = deferred(
        &done,
        r#"
  pre.scrollTop = 1000;
  nextData = done; sources[0].onopen(); await flush();
  pending[pending.length - 1].answer(LOG + "final\n"); await flush();
  out.top = pre.scrollTop;
  out.checked = follow.checked;
"#,
    ) else {
        return;
    };
    assert_eq!(out["top"], 1000, "{out}");
    assert_eq!(out["checked"], false, "{out}");
}

#[test]
fn a_response_for_a_job_closed_or_replaced_meanwhile_is_ignored() {
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let Some(out) = deferred(
        &done,
        r#"
  sources[0].onopen(); await flush();
  var slow = pending[pending.length - 1];
  var other = $("queue-body").children.filter(function (r) { return r.attrs["data-job"] === "j-queued"; })[0];
  other.fire("click"); await flush();
  pending[pending.length - 1].answer("other job\n"); await flush();
  slow.answer("old job\n"); await flush();
  out.text = pre.textContent;
"#,
    ) else {
        return;
    };
    assert_eq!(out["text"], "other job\n", "{out}");
}

#[test]
fn no_load_is_started_by_the_timer_or_a_refresh_once_the_job_is_known_finished() {
    let done = running(StepState::Passed, StepState::Passed, JobState::Passed);
    let Some(out) = deferred(
        &done,
        r#"
  nextData = done; sources[0].onopen(); await flush();
  var atFinish = pending.length;
  tickLog(); tickLog(); await flush();
  sources[0].onopen(); await flush();
  out.extra = pending.length - atFinish;
"#,
    ) else {
        return;
    };
    assert_eq!(out["extra"], 0, "{out}");
}
