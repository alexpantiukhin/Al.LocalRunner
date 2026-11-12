mod common;

use common::{err, ok};
use lr_broker::policy::ListenValue;
use std::net::IpAddr;

#[test]
fn unknown_table_refused_with_its_name_and_line() {
    let e = err("[queue]\nstrategy = \"weighted\"\n\n[queu]\nage_after_min = 5\n");
    assert_eq!(e.table.as_deref(), Some("queu"));
    assert_eq!(e.line, Some(4));
    assert!(e.message.contains("queu"), "{e}");
}

#[test]
fn unknown_key_refused_with_table_key_and_line() {
    let e = err("[grace]\nlost_min = 3\nlost_mins = 4\n");
    assert_eq!(e.table.as_deref(), Some("grace"));
    assert_eq!(e.key.as_deref(), Some("lost_mins"));
    assert_eq!(e.line, Some(3));
    let shown = e.to_string();
    assert!(shown.contains("line 3"), "{shown}");
    assert!(shown.contains("[grace]"), "{shown}");
    assert!(shown.contains("lost_mins"), "{shown}");
}

#[test]
fn unknown_key_in_every_table_refused() {
    for table in [
        "queue",
        "clients",
        "join",
        "orphans",
        "grace",
        "results",
        "listen",
        "cancel",
        "pool",
        "limits",
        "routing",
        "history",
        "saturation",
        "cache",
        "services",
        "visibility",
        "commands",
    ] {
        let e = err(&format!("[{table}]\nbogus = 1\n"));
        assert_eq!(e.table.as_deref(), Some(table), "{e}");
        assert_eq!(e.key.as_deref(), Some("bogus"), "{e}");
    }
    for array in [
        "local_user",
        "secrets.allow",
        "services.allow",
        "commands.client",
    ] {
        let e = err(&format!("[[{array}]]\nbogus = 1\n"));
        assert_eq!(e.key.as_deref(), Some("bogus"), "{array}: {e}");
    }
}

#[test]
fn syntax_error_reports_line() {
    let e = err("[queue]\nstrategy = \n");
    assert_eq!(e.line, Some(2));
}

#[test]
fn wrong_type_reports_key() {
    let e = err("[pool]\ncpu = \"eight\"\n");
    assert_eq!(e.table.as_deref(), Some("pool"));
    assert_eq!(e.key.as_deref(), Some("cpu"));
    assert_eq!(e.line, Some(2));
}

#[test]
fn unknown_enum_values_refused() {
    for (table, key, value) in [
        ("clients", "group_by", "machine"),
        ("orphans", "queued", "drop"),
        ("orphans", "running", "pause"),
        ("cancel", "others", "everyone"),
        ("cache", "share", "global"),
        ("visibility", "others_orders", "some"),
        ("commands", "default", "none"),
        ("limits", "timeout_above_max", "cut"),
    ] {
        let e = err(&format!("[{table}]\n{key} = \"{value}\"\n"));
        assert_eq!(e.key.as_deref(), Some(key), "{e}");
        assert_eq!(e.line, Some(2), "{e}");
    }
}

#[test]
fn zero_durations_and_sizes_refused() {
    for (table, key) in [
        ("join", "code_ttl_min"),
        ("orphans", "absent_after_min"),
        ("grace", "lost_min"),
        ("results", "keep_hours"),
        ("pool", "mem_mb"),
        ("pool", "cpu"),
        ("limits", "max_job_timeout_s"),
        ("limits", "job_log_max_mb"),
        ("limits", "outbox_max_mb"),
        ("limits", "max_active_per_client"),
        ("limits", "api_requests_per_min"),
        ("history", "keep_days"),
        ("history", "logs_max_gb"),
        ("history", "rows_keep_days"),
        ("saturation", "queue_wait_min"),
        ("saturation", "mem_percent"),
    ] {
        let e = err(&format!("[{table}]\n{key} = 0\n"));
        assert_eq!(e.key.as_deref(), Some(key), "{e}");
    }
}

#[test]
fn mem_percent_above_100_refused() {
    let e = err("[saturation]\nmem_percent = 101\n");
    assert_eq!(e.key.as_deref(), Some("mem_percent"));
    ok("[saturation]\nmem_percent = 100\n");
}

#[test]
fn wildcard_listen_address_refused() {
    for addr in [
        "0.0.0.0:7443",
        "0.0.0.0",
        "[::]:7443",
        "::",
        "[::ffff:0.0.0.0]:7443",
    ] {
        let e = err(&format!("[listen]\naddress = \"{addr}\"\n"));
        assert_eq!(e.table.as_deref(), Some("listen"), "{addr}");
        assert_eq!(e.key.as_deref(), Some("address"), "{addr}");
        assert_eq!(e.line, Some(2), "{addr}");
    }
}

#[test]
fn malformed_listen_address_refused() {
    let e = err("[listen]\naddress = \"my-host:7443\"\n");
    assert_eq!(e.key.as_deref(), Some("address"));
}

#[test]
fn listen_accepts_several_explicit_addresses_in_order() {
    let f = ok("[listen]\naddress = [\"192.168.1.10:7443\", \"172.17.0.1\"]\n");
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();
    assert_eq!(
        f.listen.addresses,
        [
            ListenValue::Explicit(ip("192.168.1.10"), Some(7443)),
            ListenValue::Explicit(ip("172.17.0.1"), None),
        ]
    );
}

#[test]
fn container_listen_must_stand_alone() {
    let e = err("[listen]\naddress = [\"container\", \"192.168.1.10\"]\n");
    assert_eq!(e.key.as_deref(), Some("address"));
}

#[test]
fn bad_network_range_refused_naming_the_network() {
    let e = err("[networks]\nhome = [\"192.168.1.0/33\"]\n");
    assert_eq!(e.table.as_deref(), Some("networks"));
    assert_eq!(e.key.as_deref(), Some("home"));
    assert_eq!(e.line, Some(2));
}

#[test]
fn bare_network_address_means_one_host() {
    let f = ok("[networks]\nhome = [\"192.168.1.7\", \"fd00::/64\"]\n");
    let home: Vec<String> = f.networks["home"].iter().map(|n| n.to_string()).collect();
    assert_eq!(home, ["192.168.1.7/32", "fd00::/64"]);
}

#[test]
fn routing_zone_order_rejects_unknown_and_duplicate_zones() {
    let e = err("[routing]\nrunner_zone_order = [\"local\", \"moon\"]\n");
    assert_eq!(e.key.as_deref(), Some("runner_zone_order"));
    let e = err("[routing]\nrunner_zone_order = [\"local\", \"local\"]\n");
    assert_eq!(e.key.as_deref(), Some("runner_zone_order"));
}

#[test]
fn local_user_needs_a_unique_non_empty_name() {
    let e = err("[[local_user]]\nname = \"\"\n");
    assert_eq!(e.table.as_deref(), Some("local_user"));
    assert_eq!(e.key.as_deref(), Some("name"));
    let e =
        err("[[local_user]]\nname = \"alex\"\n\n[[local_user]]\nname = \"alex\"\nlabel = \"ci\"\n");
    assert_eq!(e.key.as_deref(), Some("name"));
    assert_eq!(e.line, Some(4));
    let f = ok("[[local_user]]\nname = \"alex\"\n");
    assert_eq!(f.local_users[0].label, "");
}

#[test]
fn command_client_needs_label_or_key() {
    let e = err("[[commands.client]]\nmode = \"named\"\n");
    assert_eq!(e.table.as_deref(), Some("commands.client"));
    let f = ok("[[commands.client]]\nkey = \"k1\"\nmode = \"named\"\n");
    assert_eq!(f.commands.client[0].key.as_deref(), Some("k1"));
}

#[test]
fn allow_entry_needs_label_and_names() {
    let e = err("[[secrets.allow]]\nlabel = \"ci\"\n");
    assert_eq!(e.table.as_deref(), Some("secrets.allow"));
    let e = err("[[services.allow]]\nnames = [\"emu\"]\n");
    assert_eq!(e.table.as_deref(), Some("services.allow"));
}
