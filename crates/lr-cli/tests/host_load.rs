#![cfg(any(windows, target_os = "linux"))]

use lr_abstractions::HostLoad;
use lr_cli::host_load::SystemHostLoad;
use std::time::{Duration, Instant};

#[test]
fn the_second_reading_carries_the_machine_load() {
    let reader = SystemHostLoad::new();
    assert_eq!(
        reader.sample(),
        None,
        "the CPU share needs a previous reading"
    );
    let hang = Instant::now() + Duration::from_secs(10);
    let sample = loop {
        if let Some(sample) = reader.sample() {
            break sample;
        }
        assert!(
            Instant::now() < hang,
            "the CPU counters never moved between two readings"
        );
        std::thread::yield_now();
    };
    assert!(sample.free_mem_mb > 0);
    assert!(sample.cpu_busy_percent <= 100);
    assert!(sample.cpu_cores >= 1);
}
