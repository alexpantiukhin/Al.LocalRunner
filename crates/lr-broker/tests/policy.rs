mod common;

use common::{err, ok, HOST, SPEC_EXAMPLE};
use lr_broker::policy::{
    ActivePolicy, AllowEntry, CacheShare, CancelOthers, CommandMode, ListenValue, LocalUserEntry,
    OrphanQueued, OrphanRunning, OthersOrders, RestartOnly,
};
use lr_core::{GroupBy, Strategy, Zone};
use std::net::{IpAddr, Ipv4Addr};

fn spec_example_from_html() -> String {
    let html = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/architecture.html"
    ))
    .expect("architecture.html is readable");
    let section = html.find("<h3>3.5 Политики</h3>").expect("section 3.5");
    let open = "<pre class=\"code\">";
    let start = html[section..].find(open).expect("example block") + section + open.len();
    let end = html[start..].find("</pre>").expect("block end") + start;
    html[start..end]
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

#[test]
fn spec_fixture_matches_architecture_html() {
    assert_eq!(
        SPEC_EXAMPLE.replace("\r\n", "\n"),
        spec_example_from_html().replace("\r\n", "\n")
    );
}

#[test]
fn spec_example_policy_parses() {
    let f = ok(SPEC_EXAMPLE);
    assert_eq!(f.queue.strategy, Strategy::PriorityFifo);
    assert_eq!(f.queue.age_after_min, None);
    assert_eq!(f.rules.len(), 2);
    assert_eq!(f.rules[0].rule.name.as_deref(), Some("ci-first"));
    assert_eq!(f.rules[0].rule.when.client_label.as_deref(), Some("ci"));
    assert_eq!(f.rules[0].rule.priority, Some(100));
    assert_eq!(f.rules[1].rule.name.as_deref(), Some("ci-reserve"));
    assert_eq!(f.rules[1].rule.when.source.as_deref(), Some("github"));
    let reserve = f.rules[1].rule.reserve.expect("reserve");
    assert_eq!((reserve.cpu, reserve.mem_mb), (2, 4096));
    assert_eq!(f.clients.group_by, GroupBy::Label);
    assert_eq!(f.join.code_ttl_min.get(), 60);
    assert_eq!(f.orphans.absent_after_min.get(), 360);
    assert_eq!(f.orphans.queued, OrphanQueued::Cancel);
    assert_eq!(f.orphans.running, OrphanRunning::Finish);
    assert_eq!(f.grace.lost_min.get(), 3);
    assert_eq!(f.results.keep_hours.get(), 72);
    assert_eq!(
        f.listen.addresses,
        [ListenValue::Explicit(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
            Some(7443)
        )]
    );
    let home: Vec<String> = f.networks["home"].iter().map(|n| n.to_string()).collect();
    assert_eq!(home, ["192.168.1.0/24", "127.0.0.1/32"]);
    assert_eq!(
        f.local_users,
        [
            LocalUserEntry {
                name: "alex".into(),
                label: String::new()
            },
            LocalUserEntry {
                name: "ci-agent".into(),
                label: "ci".into()
            },
        ]
    );
    assert_eq!(f.cancel.others, CancelOthers::Nobody);
    assert_eq!(
        f.secrets.allow,
        [
            AllowEntry {
                label: "ci".into(),
                names: vec!["registry_auth".into(), "signing_key".into()]
            },
            AllowEntry {
                label: "agent".into(),
                names: vec!["registry_auth".into()]
            },
        ]
    );
    assert_eq!(f.pool.mem_mb.map(|v| v.get()), Some(16384));
    assert_eq!(f.pool.cpu.map(|v| v.get()), Some(8));
    assert!(f.pool.exclusive_tags.contains("android-emulator"));
    assert_eq!(f.limits.max_job_timeout_s.map(|v| v.get()), Some(3600));
    assert_eq!(f.limits.job_log_max_mb.get(), 256);
    assert_eq!(f.limits.outbox_max_mb.get(), 512);
    assert_eq!(f.limits.max_active_per_client.get(), 100);
    assert_eq!(f.limits.api_requests_per_min.get(), 1200);
    assert_eq!(
        f.limits.timeout_above_max,
        lr_broker::policy::TimeoutAboveMax::Reject
    );
    assert_eq!(
        f.routing.runner_zone_order,
        [Zone::Local, Zone::Lan, Zone::Remote]
    );
    assert_eq!(f.history.keep_days.get(), 180);
    assert_eq!(f.history.logs_max_gb.get(), 2);
    assert_eq!(f.history.rows_keep_days.get(), 365);
    assert_eq!(f.saturation.queue_wait_min.get(), 15);
    assert_eq!(f.saturation.mem_percent.get(), 90);
    assert_eq!(f.cache.share, CacheShare::Client);
    assert!(f.services.evict_idle);
    assert_eq!(
        f.services.allow,
        [AllowEntry {
            label: "ci".into(),
            names: vec!["gradle-daemon".into(), "emulator".into()]
        }]
    );
    assert_eq!(f.visibility.others_orders, OthersOrders::Summary);
    assert_eq!(f.commands.default, CommandMode::Any);
    assert_eq!(f.commands.client.len(), 1);
    assert_eq!(f.commands.client[0].label.as_deref(), Some("guest"));
    assert_eq!(f.commands.client[0].mode, CommandMode::Named);
}

#[test]
fn defaults_applied_grace_3_no_aging() {
    let f = ok("");
    assert_eq!(f.grace.lost_min.get(), 3);
    assert_eq!(f.queue.age_after_min, None);
    let one_key = ok("[queue]\nstrategy = \"fair-share\"\n");
    assert_eq!(one_key.grace.lost_min.get(), 3);
    assert_eq!(one_key.queue.age_after_min, None);
}

#[test]
fn listen_change_on_reload_reported_not_applied() {
    let (mut active, _) =
        ActivePolicy::start("[listen]\naddress = \"192.168.1.10:7443\"\n", HOST, false).unwrap();
    let applied = active
        .reload("[listen]\naddress = \"10.0.0.5:7443\"\n")
        .unwrap();
    assert_eq!(applied.restart_required, [RestartOnly::Listen]);
    assert_eq!(
        active.file().listen.addresses,
        [ListenValue::Explicit(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
            Some(7443)
        )]
    );
}

#[test]
fn invalid_reload_keeps_previous() {
    let (mut active, _) = ActivePolicy::start("[grace]\nlost_min = 7\n", HOST, false).unwrap();
    let e = active.reload("[grace]\nlost_mni = 9\n").unwrap_err();
    assert_eq!(e.table.as_deref(), Some("grace"));
    assert_eq!(active.file().grace.lost_min.get(), 7);
    let e = active
        .reload("[queue]\nstrategy = \"random\"\n")
        .unwrap_err();
    assert_eq!(e.key.as_deref(), Some("strategy"));
    assert_eq!(active.file().grace.lost_min.get(), 7);
    assert_eq!(active.file().queue.strategy, Strategy::PriorityFifo);
}

#[test]
fn rule_without_name_accepted() {
    let f = ok("[[rule]]\nwhen = { client_label = \"ci\" }\npriority = 5\n");
    assert_eq!(f.rules.len(), 1);
    assert_eq!(f.rules[0].rule.name, None);
    assert_eq!(f.rules[0].rule.priority, Some(5));
    assert_eq!(lr_broker::policy::rule_policy(&f, &HOST).rule_name(0), "1");
}

#[test]
fn container_listen_value_accepted() {
    let f = ok("[listen]\naddress = \"container\"\n");
    assert_eq!(f.listen.addresses, [ListenValue::Container]);
    assert!(f.listen.check_at_start(true).is_ok());
    let e = f.listen.check_at_start(false).unwrap_err();
    assert_eq!(e.table.as_deref(), Some("listen"));
    assert_eq!(e.key.as_deref(), Some("address"));
}

#[test]
fn service_start_refuses_container_listen_outside_a_container() {
    let text = "[listen]\naddress = \"container\"\n";
    let e = ActivePolicy::start(text, HOST, false).unwrap_err();
    assert_eq!(e.table.as_deref(), Some("listen"));
    assert_eq!(e.key.as_deref(), Some("address"));
    let (active, _) = ActivePolicy::start(text, HOST, true).unwrap();
    assert_eq!(active.file().listen.addresses, [ListenValue::Container]);
}

#[test]
fn empty_file_does_not_break_the_system() {
    let f = ok("");
    let p = lr_broker::policy::into_policy(&f, &HOST);
    assert_eq!(p.pool().mem_mb, HOST.mem_mb);
    assert_eq!(p.pool().cpu, HOST.cpu);
}

#[test]
fn unknown_strategy_refused() {
    let e = err("[queue]\nstrategy = \"lottery\"\n");
    assert_eq!(e.table.as_deref(), Some("queue"));
    assert_eq!(e.key.as_deref(), Some("strategy"));
    assert_eq!(e.line, Some(2));
    assert!(e.message.contains("lottery"), "{e}");
}

#[test]
fn every_builtin_strategy_name_loads() {
    for (name, want) in [
        ("priority-fifo", Strategy::PriorityFifo),
        ("fair-share", Strategy::FairShare),
        ("weighted", Strategy::Weighted),
    ] {
        let f = ok(&format!("[queue]\nstrategy = \"{name}\"\n"));
        assert_eq!(f.queue.strategy, want);
    }
}
