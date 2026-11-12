#![cfg(any(windows, target_os = "linux"))]

use lr_abstractions::HostLoad;
use lr_cli::host_load::SystemHostLoad;
use std::time::Duration;

#[test]
fn the_second_reading_carries_the_machine_load() {
    let reader = SystemHostLoad::new();
    assert_eq!(
        reader.sample(),
        None,
        "the CPU share needs a previous reading"
    );
    std::thread::sleep(Duration::from_millis(300));
    let sample = reader.sample().expect("the host load is readable");
    assert!(sample.free_mem_mb > 0);
    assert!(sample.cpu_busy_percent <= 100);
    assert!(sample.cpu_cores >= 1);
}
