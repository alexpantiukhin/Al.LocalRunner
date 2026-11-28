use lr_cli::commands::restart_cmd::{restart_with, runner_names_only};
use std::sync::atomic::{AtomicBool, Ordering};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn restart_takes_runners_and_refuses_the_hub_the_broker_and_no_names() {
    assert!(runner_names_only(&names(&["r1", "local:r2", "runner"])).is_ok());
    let none = runner_names_only(&[]).unwrap_err();
    assert!(none.contains("name the runners"), "{none}");
    for role in ["hub", "broker", "all"] {
        let err = runner_names_only(&names(&["r1", role])).unwrap_err();
        assert!(err.contains("runners only") && err.contains("down"), "{err}");
    }
}

#[tokio::test]
async fn restart_starts_the_runners_again_only_after_they_stopped() {
    let started = AtomicBool::new(false);
    let code = restart_with(
        &names(&["r1"]),
        async { Ok(0) },
        async {
            started.store(true, Ordering::SeqCst);
            Ok(0)
        },
    )
    .await;
    assert_eq!(code, Ok(0));
    assert!(started.load(Ordering::SeqCst));

    let started = AtomicBool::new(false);
    let code = restart_with(
        &names(&["r1"]),
        async { Ok(1) },
        async {
            started.store(true, Ordering::SeqCst);
            Ok(0)
        },
    )
    .await;
    assert_eq!(code, Ok(1));
    assert!(
        !started.load(Ordering::SeqCst),
        "a runner that did not stop is not started a second time"
    );
}
