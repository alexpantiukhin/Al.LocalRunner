mod common;

use common::{job, pool, req, set};
use lr_core::{Free, PoolLimits, Reject, Scheduler, Wait};

#[test]
fn grant_release_restores_free() {
    let mut s = Scheduler::new(pool(8192, 8));
    s.try_grant(&job("a"), &req(3000, 3, 60)).unwrap();
    assert_eq!(
        s.free(),
        Free {
            mem_mb: 5192,
            cpu: 5
        }
    );
    s.release(&job("a"));
    assert_eq!(
        s.free(),
        Free {
            mem_mb: 8192,
            cpu: 8
        }
    );
    s.release(&job("unknown"));
    assert_eq!(s.free().mem_mb, 8192);
}

#[test]
fn memory_and_cpu_waits_carry_numbers() {
    let mut s = Scheduler::new(pool(8192, 8));
    s.try_grant(&job("a"), &req(6000, 2, 60)).unwrap();
    assert_eq!(
        s.try_grant(&job("b"), &req(4096, 1, 60)),
        Err(Wait::Memory {
            needed: 4096,
            free: 2192
        })
    );
    assert_eq!(
        s.try_grant(&job("c"), &req(1024, 7, 60)),
        Err(Wait::Cpu { needed: 7, free: 6 })
    );
    assert!(!s.holds(&job("b")));
    assert_eq!(s.free().mem_mb, 2192);
}

#[test]
fn grant_is_all_or_nothing() {
    let mut s = Scheduler::new(pool(8192, 2));
    assert!(s.try_grant(&job("a"), &req(1024, 3, 60)).is_err());
    assert_eq!(
        s.free(),
        Free {
            mem_mb: 8192,
            cpu: 2
        }
    );
}

#[test]
fn second_grant_of_the_same_job_is_a_no_op() {
    let mut s = Scheduler::new(pool(8192, 8));
    s.try_grant(&job("a"), &req(4096, 4, 60)).unwrap();
    s.try_grant(&job("a"), &req(4096, 4, 60)).unwrap();
    assert_eq!(s.free().mem_mb, 4096);
}

#[test]
fn exclusive_tag_blocks_and_names_holder() {
    let mut s = Scheduler::new(pool(8192, 8));
    let mut holder = req(1024, 1, 60);
    holder.tags = set(&["emulator"]);
    holder.exclusive = set(&["emulator"]);
    s.try_grant(&job("holder"), &holder).unwrap();
    let mut other = req(1024, 1, 60);
    other.tags = set(&["emulator"]);
    let err = s.try_grant(&job("other"), &other).unwrap_err();
    assert_eq!(
        err,
        Wait::Exclusive {
            tag: "emulator".into(),
            holder: job("holder")
        }
    );
    assert!(err.to_string().contains("holder"));
    s.release(&job("holder"));
    assert!(s.try_grant(&job("other"), &other).is_ok());
}

#[test]
fn exclusive_request_waits_for_a_shared_holder() {
    let mut s = Scheduler::new(pool(8192, 8));
    let mut shared = req(1024, 1, 60);
    shared.tags = set(&["gradle"]);
    s.try_grant(&job("shared"), &shared).unwrap();
    s.try_grant(&job("shared2"), &shared).unwrap();
    let mut excl = req(1024, 1, 60);
    excl.exclusive = set(&["gradle"]);
    assert!(matches!(
        s.try_grant(&job("excl"), &excl),
        Err(Wait::Exclusive { .. })
    ));
}

#[test]
fn pool_exclusive_tags_apply_without_order_marking() {
    let mut s = Scheduler::new(PoolLimits {
        exclusive_tags: set(&["android-emulator"]),
        ..pool(8192, 8)
    });
    let mut r = req(1024, 1, 60);
    r.tags = set(&["android-emulator"]);
    s.try_grant(&job("a"), &r).unwrap();
    assert_eq!(
        s.try_grant(&job("b"), &r),
        Err(Wait::Exclusive {
            tag: "android-emulator".into(),
            holder: job("a")
        })
    );
}

#[test]
fn larger_than_pool_never_fits() {
    let s = Scheduler::new(pool(8192, 8));
    let err = s.can_ever_fit(&req(12288, 2, 60)).unwrap_err();
    assert_eq!(
        err,
        Reject::PoolMemory {
            needed: 12288,
            limit: 8192
        }
    );
    assert_eq!(err.to_string(), "нужно 12288 МБ, максимум в пуле 8192");
    assert_eq!(
        s.can_ever_fit(&req(1024, 9, 60)),
        Err(Reject::PoolCpu {
            needed: 9,
            limit: 8
        })
    );
    assert!(s.can_ever_fit(&req(8192, 8, 60)).is_ok());
}

#[test]
fn insert_running_records_without_checks() {
    let mut s = Scheduler::new(pool(1024, 1));
    s.insert_running(&job("a"), &req(2048, 2, 60));
    assert_eq!(s.free(), Free { mem_mb: 0, cpu: 0 });
    s.release(&job("a"));
    assert_eq!(s.free().mem_mb, 1024);
}
