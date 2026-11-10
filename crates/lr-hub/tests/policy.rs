use lr_core::Zone;
use lr_hub::{parse_hub_policy, BrokerStrategy, HubPolicy, OnExpire, ZoneQuery};

#[test]
fn empty_file_gives_the_defaults() {
    let p = parse_hub_policy("").unwrap();
    assert_eq!(p, HubPolicy::default());
    assert!(p.brokers.order.is_empty());
    assert_eq!(p.brokers.strategy, BrokerStrategy::First);
    assert_eq!(
        p.brokers.broker_zone_order,
        vec![Zone::Local, Zone::Lan, Zone::Remote]
    );
    assert_eq!(p.brokers.query_timeout_s, 3);
    assert_eq!(p.brokers.query_attempts, 3);
    assert_eq!(p.brokers.zone_query, ZoneQuery::Sequential);
    assert_eq!(p.brokers.query_retry_s, 5);
    assert_eq!(p.outbox.ttl_hours, 24);
    assert_eq!(p.outbox.on_expire, OnExpire::Fail);
}

#[test]
fn every_key_of_the_spec_example_is_read() {
    let p = parse_hub_policy(
        r#"
[brokers]
order             = ["win-host", "linux-box"]
strategy          = "least_loaded"
broker_zone_order = ["remote", "lan", "local"]
query_timeout_s   = 7
query_attempts    = 4
zone_query        = "all"
query_retry_s     = 9

[outbox]
ttl_hours = 48
on_expire = "keep"
"#,
    )
    .unwrap();
    assert_eq!(p.brokers.order, vec!["win-host", "linux-box"]);
    assert_eq!(p.brokers.strategy, BrokerStrategy::LeastLoaded);
    assert_eq!(
        p.brokers.broker_zone_order,
        vec![Zone::Remote, Zone::Lan, Zone::Local]
    );
    assert_eq!(
        (
            p.brokers.query_timeout_s,
            p.brokers.query_attempts,
            p.brokers.query_retry_s
        ),
        (7, 4, 9)
    );
    assert_eq!(p.brokers.zone_query, ZoneQuery::All);
    assert_eq!(p.outbox.ttl_hours, 48);
    assert_eq!(p.outbox.on_expire, OnExpire::Keep);
}

#[test]
fn unknown_key_or_table_is_refused_with_its_name_and_line() {
    let e = parse_hub_policy("[brokers]\nordr = []\n")
        .unwrap_err()
        .to_string();
    assert!(e.contains("ordr") && e.contains("line 2"), "{e}");
    let e = parse_hub_policy("[queue]\n").unwrap_err().to_string();
    assert!(e.contains("queue"), "{e}");
}

#[test]
fn unknown_values_are_refused() {
    for text in [
        "[brokers]\nstrategy = \"random\"",
        "[brokers]\nzone_query = \"some\"",
        "[outbox]\non_expire = \"retry\"",
        "[brokers]\nbroker_zone_order = [\"local\", \"lan\"]",
        "[brokers]\nbroker_zone_order = [\"local\", \"lan\", \"lan\"]",
        "[brokers]\nbroker_zone_order = [\"local\", \"lan\", \"moon\"]",
        "[brokers]\norder = [\"a\", \"a\"]",
        "[brokers]\nquery_timeout_s = 0",
        "[brokers]\nquery_attempts = 0",
        "[brokers]\nquery_timeout_s = 3601",
        "[outbox]\nttl_hours = 0",
        "[outbox]\nttl_hours = 8761",
    ] {
        assert!(parse_hub_policy(text).is_err(), "accepted: {text}");
    }
}

#[test]
fn bounds_are_inclusive() {
    let p = parse_hub_policy(
        "[brokers]\nquery_timeout_s = 3600\nquery_retry_s = 0\nquery_attempts = 100\n[outbox]\nttl_hours = 8760",
    )
    .unwrap();
    assert_eq!(p.brokers.query_timeout_s, 3600);
    assert_eq!(p.brokers.query_retry_s, 0);
    assert_eq!(p.brokers.query_attempts, 100);
    assert_eq!(p.outbox.ttl_hours, 8760);
    assert!(parse_hub_policy("[brokers]\nquery_attempts = 101").is_err());
}
