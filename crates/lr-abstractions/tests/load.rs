use lr_abstractions::fakes::FakeHostLoad;
use lr_abstractions::load::{busy_percent, mem_available_mb, proc_stat_times, CpuTimes};
use lr_abstractions::{HostLoad, LoadSample};

#[test]
fn memory_available_is_read_from_meminfo() {
    let text =
        "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:    8192000 kB\n";
    assert_eq!(mem_available_mb(text), Some(8000));
    assert_eq!(mem_available_mb("MemTotal: 1 kB\n"), None);
    assert_eq!(mem_available_mb("MemAvailable: lots kB\n"), None);
}

#[test]
fn the_cpu_line_of_proc_stat_counts_iowait_as_idle() {
    let stat = "cpu  100 10 50 800 40 0 0 0 5 0\ncpu0 1 1 1 1 1 0 0 0 0 0\n";
    assert_eq!(
        proc_stat_times(stat),
        Some(CpuTimes {
            idle: 840,
            total: 1000
        })
    );
    assert_eq!(proc_stat_times("cpu0 1 1 1 1\n"), None);
    assert_eq!(proc_stat_times("cpu  1 2 3\n"), None);
}

#[test]
fn busy_share_comes_from_the_difference_of_two_readings() {
    let a = CpuTimes {
        idle: 800,
        total: 1000,
    };
    let b = CpuTimes {
        idle: 850,
        total: 1100,
    };
    assert_eq!(busy_percent(a, b), Some(50));
    assert_eq!(busy_percent(a, a), None);
    let idle = CpuTimes {
        idle: 900,
        total: 1100,
    };
    assert_eq!(busy_percent(a, idle), Some(0));
    let full = CpuTimes {
        idle: 800,
        total: 1100,
    };
    assert_eq!(busy_percent(a, full), Some(100));
    let backwards = CpuTimes { idle: 0, total: 10 };
    assert_eq!(busy_percent(a, backwards), None);
}

#[test]
fn the_fake_returns_what_it_was_given() {
    let fake = FakeHostLoad::new();
    assert_eq!(fake.sample(), None);
    let sample = LoadSample {
        free_mem_mb: 1,
        cpu_busy_percent: 2,
        cpu_cores: 3,
    };
    fake.set(Some(sample));
    assert_eq!(fake.clone().sample(), Some(sample));
}
