use lr_core::MachineLoad;
use lr_proto::*;
use serde_json::json;

fn parse(text: &str) -> RunnerToBroker {
    decode::<RunnerToBroker>(text.as_bytes()).unwrap()
}

#[test]
fn a_heartbeat_of_an_older_runner_parses_without_a_load() {
    assert_eq!(
        parse(r#"{"type":"heartbeat"}"#),
        RunnerToBroker::Heartbeat { load: None }
    );
}

#[test]
fn a_heartbeat_without_a_load_is_sent_as_before() {
    assert_eq!(
        serde_json::to_value(RunnerToBroker::Heartbeat { load: None }).unwrap(),
        json!({"type": "heartbeat"})
    );
}

#[test]
fn a_heartbeat_with_a_load_round_trips() {
    let beat = RunnerToBroker::Heartbeat {
        load: Some(WireLoad {
            free_mem_mb: 4096,
            cpu_busy_percent: 37,
            cpu_cores: 16,
        }),
    };
    let bytes = encode(&beat).unwrap();
    assert_eq!(decode::<RunnerToBroker>(&bytes).unwrap(), beat);
    assert_eq!(
        serde_json::to_value(&beat).unwrap(),
        json!({"type": "heartbeat", "load": {"free_mem_mb": 4096, "cpu_busy_percent": 37, "cpu_cores": 16}})
    );
}

#[test]
fn the_load_converts_for_the_scheduler_and_clamps_the_percent() {
    let load: MachineLoad = WireLoad {
        free_mem_mb: 10,
        cpu_busy_percent: 250,
        cpu_cores: 4,
    }
    .into();
    assert_eq!(
        load,
        MachineLoad {
            free_mem_mb: 10,
            cpu_busy_percent: 100,
            cpu_cores: 4
        }
    );
}
