mod common;

use common::{entry, ok, HOST};
use lr_broker::policy::{
    into_policy, rule_policy, CacheShare, CancelOthers, CommandMode, OrphanQueued, OrphanRunning,
    OthersOrders, DEFAULT_CI_PRIORITY,
};
use lr_core::{GroupBy, ServeLog, Strategy, Timestamp, Zone};

#[test]
fn missing_rules_yield_default_ci_first_rule() {
    let f = ok("");
    assert_eq!(f.rules.len(), 1);
    let rule = &f.rules[0].rule;
    assert_eq!(rule.name.as_deref(), Some("ci-first"));
    assert_eq!(rule.when.client_label.as_deref(), Some("ci"));
    assert_eq!(rule.priority, Some(DEFAULT_CI_PRIORITY));
    assert_eq!(DEFAULT_CI_PRIORITY, 100);
}

#[test]
fn default_policy_puts_ci_orders_first() {
    let p = into_policy(&ok(""), &HOST);
    let queue = [entry("old", "agent", 0), entry("ci", "ci", 5_000)];
    let order = p.order(&queue, &ServeLog::default(), Timestamp(10_000));
    assert_eq!(order, [1, 0]);
}

#[test]
fn explicit_empty_rule_list_means_no_rules() {
    let f = ok("rule = []\n");
    assert!(f.rules.is_empty());
    let p = rule_policy(&f, &HOST);
    assert_eq!(p.priority_of(&entry("ci", "ci", 0)), 0);
}

#[test]
fn own_rules_replace_the_default_rule() {
    let f = ok("[[rule]]\nwhen = { client_label = \"night\" }\npriority = 1\n");
    assert_eq!(f.rules.len(), 1);
    assert_eq!(f.rules[0].rule.when.client_label.as_deref(), Some("night"));
}

#[test]
fn default_queue_is_priority_fifo_aging_after_30_min() {
    let f = ok("");
    assert_eq!(f.queue.strategy, Strategy::PriorityFifo);
    assert_eq!(f.queue.age_after_min, 30);
    assert_eq!(rule_policy(&f, &HOST).age_after_min, 30);
}

#[test]
fn default_groups_by_label() {
    assert_eq!(ok("").clients.group_by, GroupBy::Label);
}

#[test]
fn default_join_code_lives_one_hour() {
    assert_eq!(ok("").join.code_ttl_min.get(), 60);
}

#[test]
fn default_orphans_cancel_queued_and_finish_running_after_six_hours() {
    let f = ok("");
    assert_eq!(f.orphans.absent_after_min.get(), 360);
    assert_eq!(f.orphans.queued, OrphanQueued::Cancel);
    assert_eq!(f.orphans.running, OrphanRunning::Finish);
}

#[test]
fn default_grace_is_three_minutes_for_runners() {
    let f = ok("");
    assert_eq!(f.grace.lost_min.get(), 3);
    assert_eq!(f.settings_update().broker_lost_kill_s, Some(180));
}

#[test]
fn default_results_kept_72_hours() {
    assert_eq!(ok("").results.keep_hours.get(), 72);
}

#[test]
fn default_listens_nowhere_and_knows_no_networks() {
    let f = ok("");
    assert!(f.listen.addresses.is_empty());
    assert!(f.networks.is_empty());
    assert!(f.listen.check_at_start(false).is_ok());
}

#[test]
fn default_local_user_list_is_empty() {
    assert!(ok("").local_users.is_empty());
}

#[test]
fn default_cancel_of_others_is_owner_only() {
    assert_eq!(ok("").cancel.others, CancelOthers::Owner);
}

#[test]
fn default_allows_no_secrets() {
    let f = ok("");
    assert!(f.secrets_allowed("ci").is_empty());
    assert!(f.secrets_allowed("").is_empty());
}

#[test]
fn default_pool_is_the_whole_host() {
    let f = ok("");
    assert_eq!(f.pool.mem_mb, None);
    assert_eq!(f.pool.cpu, None);
    let limits = f.pool.limits(&HOST);
    assert_eq!((limits.mem_mb, limits.cpu), (HOST.mem_mb, HOST.cpu));
    assert!(limits.exclusive_tags.is_empty());
    let half = ok("[pool]\ncpu = 4\n").pool.limits(&HOST);
    assert_eq!((half.mem_mb, half.cpu), (HOST.mem_mb, 4));
}

#[test]
fn default_limits_have_no_timeout_ceiling_and_spec_ceilings() {
    let f = ok("");
    assert_eq!(f.limits.max_job_timeout_s, None);
    assert_eq!(rule_policy(&f, &HOST).max_job_timeout_s, None);
    assert_eq!(f.limits.job_log_max_mb.get(), 256);
    assert_eq!(f.limits.outbox_max_mb.get(), 512);
    assert_eq!(f.limits.max_active_per_client.get(), 100);
}

#[test]
fn default_runner_zone_order_is_local_lan_remote() {
    let f = ok("");
    let want = [Zone::Local, Zone::Lan, Zone::Remote];
    assert_eq!(f.routing.runner_zone_order, want);
    assert_eq!(into_policy(&f, &HOST).runner_zone_order(), want);
}

#[test]
fn default_history_keeps_logs_30_days_2_gb_rows_365_days() {
    let f = ok("");
    assert_eq!(f.history.keep_days.get(), 30);
    assert_eq!(f.history.logs_max_gb.get(), 2);
    assert_eq!(f.history.rows_keep_days.get(), 365);
}

#[test]
fn default_saturation_thresholds_15_min_90_percent() {
    let f = ok("");
    assert_eq!(f.saturation.queue_wait_min.get(), 15);
    assert_eq!(f.saturation.mem_percent.get(), 90);
}

#[test]
fn default_cache_is_per_client() {
    assert_eq!(ok("").cache.share, CacheShare::Client);
}

#[test]
fn default_services_evict_idle_and_allow_any_label() {
    let f = ok("");
    assert!(f.services.evict_idle);
    assert!(f.service_allowed("guest", "build-daemon"));
}

#[test]
fn services_list_limits_who_may_request() {
    let f = ok("[[services.allow]]\nlabel = \"ci\"\nnames = [\"emulator\"]\n");
    assert!(f.service_allowed("ci", "emulator"));
    assert!(!f.service_allowed("ci", "build-daemon"));
    assert!(!f.service_allowed("agent", "emulator"));
}

#[test]
fn default_visibility_of_others_is_summary() {
    assert_eq!(ok("").visibility.others_orders, OthersOrders::Summary);
}

#[test]
fn default_commands_trust_the_client() {
    let f = ok("");
    assert_eq!(f.commands.default, CommandMode::Any);
    assert_eq!(f.command_mode("any-key", "agent"), CommandMode::Any);
}
