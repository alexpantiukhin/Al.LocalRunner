mod common;

use common::*;
use lr_core::Requirements;
use lr_proto::*;
use serde_json::Value;

fn without(mut v: Value, keys: &[&str]) -> Value {
    for k in keys {
        v.as_object_mut().unwrap().remove(*k);
    }
    v
}

#[test]
fn assignment_of_an_older_broker_has_no_named_commands_and_no_shared_cache() {
    let new = serde_json::to_value(assign_spec()).unwrap();
    assert!(new.get("named_commands").is_none(), "{new}");
    assert!(new.get("cache_client").is_none(), "{new}");
    let old = without(new, &["named_commands", "cache_client"]);
    let spec: AssignSpec = serde_json::from_value(old).unwrap();
    assert!(!spec.named_commands);
    assert_eq!(spec.cache_client, None);
    let shared = AssignSpec {
        named_commands: true,
        cache_client: Some(SHARED_CACHE_CLIENT.into()),
        ..assign_spec()
    };
    let v = serde_json::to_value(&shared).unwrap();
    assert_eq!(v["named_commands"], true);
    assert_eq!(v["cache_client"], "shared:project");
    assert_eq!(serde_json::from_value::<AssignSpec>(v).unwrap(), shared);
}

#[test]
fn capabilities_and_requirements_of_older_senders_have_no_command_names() {
    let new = serde_json::to_value(caps()).unwrap();
    assert!(new.get("commands").is_none(), "{new}");
    let old: WireCapabilities = serde_json::from_value(without(new, &["commands"])).unwrap();
    assert!(old.commands.is_empty());
    let req = serde_json::to_value(requirements()).unwrap();
    assert!(req.get("commands").is_none(), "{req}");
    let old: Requirements = serde_json::from_value(without(req, &["commands"])).unwrap();
    assert!(old.commands.is_empty());
    let named = WireCapabilities {
        commands: ["build".to_string()].into(),
        ..caps()
    };
    let v = serde_json::to_value(&named).unwrap();
    assert_eq!(v["commands"], serde_json::json!(["build"]));
    let back: WireCapabilities = serde_json::from_value(v).unwrap();
    assert_eq!(back, named);
}
