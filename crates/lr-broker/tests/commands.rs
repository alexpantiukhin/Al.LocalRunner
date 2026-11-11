mod common;

use common::harness::*;
use lr_core::{JobId, StepSpec};
use lr_proto::*;
use std::collections::BTreeSet;

const GUEST_NAMED: &str = "[[commands.client]]\nlabel = \"guest\"\nmode = \"named\"\n";

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn naming(list: &[&str]) -> WireCapabilities {
    WireCapabilities {
        commands: names(list),
        ..linux()
    }
}

fn step(command: &str) -> StepSpec {
    StepSpec {
        name: "s".into(),
        command: command.into(),
        timeout_s: 60,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: false,
    }
}

fn order_of(job: &str, commands: &[&str]) -> HubToBroker {
    let HubToBroker::Order(mut o) = order(job, req(512, 1, 60)) else {
        unreachable!()
    };
    o.spec.steps = commands.iter().map(|c| step(c)).collect();
    HubToBroker::Order(o)
}

#[tokio::test]
async fn named_order_runs_on_a_runner_that_names_its_commands() {
    let h = Harness::start(GUEST_NAMED);
    let mut r = h
        .member_runner("r1", "10.0.0.7", naming(&["build", "test"]))
        .await;
    let mut guest = h.member_hub("visitor", "guest", "10.0.0.22").await;
    guest.send(&order_of("j1", &["build", "test"])).await;
    guest.accepted().await;
    let a = r.assign().await;
    assert!(a.spec.named_commands);
    assert_eq!(a.spec.requirements.commands, names(&["build", "test"]));
    let sent: Vec<&str> = a.spec.steps.iter().map(|s| s.command.as_str()).collect();
    assert_eq!(sent, ["build", "test"]);
}

#[tokio::test]
async fn named_order_skips_a_runner_without_the_name() {
    let h = Harness::start(GUEST_NAMED);
    let mut plain = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut named = h.member_runner("r2", "10.0.0.8", naming(&["build"])).await;
    let mut guest = h.member_hub("visitor", "guest", "10.0.0.22").await;
    guest.send(&order_of("j1", &["build"])).await;
    guest.accepted().await;
    assert_eq!(named.assign().await.job, JobId("j1".into()));
    plain.quiet().await;
}

#[tokio::test]
async fn a_name_outside_every_runner_list_is_rejected_with_the_names() {
    let h = Harness::start(GUEST_NAMED);
    let _r = h
        .member_runner("r1", "10.0.0.7", naming(&["build", "test"]))
        .await;
    let mut guest = h.member_hub("visitor", "guest", "10.0.0.22").await;
    guest.send(&order_of("j1", &["build", "rm -rf /"])).await;
    let r = guest.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_command_not_allowed");
    assert_eq!(
        r.detail,
        "команда rm -rf / не входит в названия раннеров: ярлыку guest разрешены только \
         названия из списка раннера (названия раннеров: build, test)"
    );
    assert!(h.job_row("visitor", "j1").await.is_none());
}

#[tokio::test]
async fn named_mode_rejects_orders_when_no_runner_names_commands() {
    let h = Harness::start(GUEST_NAMED);
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut guest = h.member_hub("visitor", "guest", "10.0.0.22").await;
    guest.send(&order("j1", req(512, 1, 60))).await;
    let r = guest.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_command_not_allowed");
    assert!(r.detail.contains("make"), "{}", r.detail);
    assert!(
        r.detail
            .contains("раннеры этого брокера названий не сообщают"),
        "{}",
        r.detail
    );
    let mut agent = h.member_hub("laptop", "agent", "10.0.0.20").await;
    agent.send(&order("j1", req(512, 1, 60))).await;
    agent.accepted().await;
}

#[tokio::test]
async fn named_mode_rejects_an_order_without_steps() {
    let h = Harness::start(GUEST_NAMED);
    let _r = h.member_runner("r1", "10.0.0.7", naming(&["build"])).await;
    let mut guest = h.member_hub("visitor", "guest", "10.0.0.22").await;
    guest.send(&order_of("j1", &[])).await;
    let r = guest.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_command_not_allowed");
    assert_eq!(
        r.detail,
        "заказ без шагов не называет команд: ярлыку guest разрешены только названия из списка \
         раннера"
    );
}

#[tokio::test]
async fn a_client_trusted_with_any_command_never_gets_names_from_its_order() {
    let h = Harness::start(GUEST_NAMED);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut agent = h.member_hub("laptop", "agent", "10.0.0.20").await;
    let mut needs = req(512, 1, 60);
    needs.commands = names(&["make"]);
    agent.send(&order("j1", needs)).await;
    agent.accepted().await;
    let a = r.assign().await;
    assert!(!a.spec.named_commands);
    assert!(a.spec.requirements.commands.is_empty());
    assert_eq!(a.spec.steps[0].command, "make");
}

#[tokio::test]
async fn a_key_entry_limits_only_that_client() {
    let h = Harness::start("[[commands.client]]\nkey = \"visitor\"\nmode = \"named\"\n");
    let mut r = h.member_runner("r1", "10.0.0.7", naming(&["make"])).await;
    let mut visitor = h.member_hub("visitor", "agent", "10.0.0.22").await;
    visitor.send(&order("j1", req(512, 1, 60))).await;
    visitor.accepted().await;
    assert!(r.assign().await.spec.named_commands);
    let mut laptop = h.member_hub("laptop", "agent", "10.0.0.20").await;
    laptop.send(&order("j2", req(512, 1, 60))).await;
    laptop.accepted().await;
    r.send(&started("j1", 1)).await;
    r.send(&result("j1", 2, lr_core::JobState::Passed, 0)).await;
    assert!(!r.assign().await.spec.named_commands);
}

async fn rejoin(h: &Harness, name: &str, caps: WireCapabilities) -> Conn {
    let mut c = h.dial(member_peer(&format!("pk-{name}"), "10.0.0.7")).await;
    c.send(&hello(name, Some(caps))).await;
    c.welcome().await;
    c
}

async fn named_order_waiting_for_an_offline_runner(h: &Harness) {
    let r = h.member_runner("r1", "10.0.0.7", naming(&["build"])).await;
    r.close().await;
    let mut guest = h.member_hub("visitor", "guest", "10.0.0.22").await;
    guest.until_runner("r1", false).await;
    guest.send(&order_of("j1", &["build"])).await;
    guest.accepted().await;
}

fn assert_named(a: &Assign) {
    assert!(a.spec.named_commands);
    assert_eq!(a.spec.requirements.commands, names(&["build"]));
    assert_eq!(a.spec.steps[0].command, "build");
}

#[tokio::test]
async fn an_accepted_named_order_stays_named_after_a_restart_and_a_reload_to_any() {
    let h = Harness::start(GUEST_NAMED);
    named_order_waiting_for_an_offline_runner(&h).await;
    let h = h.restart().await;
    h.handle
        .reload_policy(&policy(""))
        .await
        .expect("reload applies");
    let mut back = rejoin(&h, "r1", naming(&["build"])).await;
    assert_named(&back.assign().await);
}

#[tokio::test]
async fn policy_reload_does_not_revisit_an_accepted_named_order() {
    let h = Harness::start(GUEST_NAMED);
    named_order_waiting_for_an_offline_runner(&h).await;
    h.handle
        .reload_policy(&policy(""))
        .await
        .expect("reload applies");
    let mut back = rejoin(&h, "r1", naming(&["build"])).await;
    assert_named(&back.assign().await);
}

#[tokio::test]
async fn after_a_restart_a_named_order_is_told_the_runners_have_not_returned() {
    let h = Harness::start(GUEST_NAMED);
    let r = h.member_runner("r1", "10.0.0.7", naming(&["build"])).await;
    r.close().await;
    let h = h.restart().await;
    let mut guest = h.member_hub("visitor", "guest", "10.0.0.22").await;
    guest.send(&order_of("j1", &["build"])).await;
    let r = guest.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_command_not_allowed");
    assert_eq!(
        r.detail,
        "команда build не входит в названия раннеров: ярлыку guest разрешены только названия \
         из списка раннера (раннеры ещё не вернулись после перезапуска брокера, повторите)"
    );
    let _back = rejoin(&h, "r1", naming(&["build"])).await;
    guest.send(&order_of("j2", &["build"])).await;
    guest.accepted().await;
}

#[tokio::test]
async fn names_split_between_runners_are_rejected_as_no_matching_runner() {
    let h = Harness::start(GUEST_NAMED);
    let _r1 = h.member_runner("r1", "10.0.0.7", naming(&["build"])).await;
    let _r2 = h.member_runner("r2", "10.0.0.8", naming(&["test"])).await;
    let mut guest = h.member_hub("visitor", "guest", "10.0.0.22").await;
    guest.send(&order_of("j1", &["build", "test"])).await;
    let r = guest.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_no_matching_runner");
    assert!(r.detail.contains("команда test не описана"), "{}", r.detail);
    assert!(
        r.detail.contains("команда build не описана"),
        "{}",
        r.detail
    );
}
