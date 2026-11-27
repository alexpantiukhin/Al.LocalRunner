use lr_abstractions::{CacheEntry, CacheKey};
use lr_cli::commands::runner_cache_cmd::cleaned_line;
use lr_core::Timestamp;
use lr_runner::plan_clean;

const DAY: u64 = 86_400_000;

fn entry(name: &str, last_used: u64) -> CacheEntry {
    CacheEntry {
        key: CacheKey {
            client: "c".into(),
            project: "p".into(),
            name: name.into(),
        },
        path: format!("/cache/{name}"),
        bytes: 1024 * 1024,
        last_used: Timestamp(last_used),
    }
}

#[test]
fn without_an_age_every_cache_not_in_use_is_cleaned() {
    let now = 100 * DAY;
    let entries = vec![entry("a", now - 30 * DAY), entry("b", now), entry("c", now)];
    let busy = vec![entries[2].key.clone()];
    let names: Vec<_> = plan_clean(&entries, Timestamp(now), None, &busy)
        .into_iter()
        .map(|e| e.key.name)
        .collect();
    assert_eq!(names, ["a", "b"]);
}

#[test]
fn with_an_age_only_the_older_caches_are_cleaned() {
    let now = 100 * DAY;
    let entries = vec![entry("old", now - 10 * DAY), entry("fresh", now - 2 * DAY)];
    let names: Vec<_> = plan_clean(&entries, Timestamp(now), Some(7), &[])
        .into_iter()
        .map(|e| e.key.name)
        .collect();
    assert_eq!(names, ["old"]);
}

#[test]
fn the_line_says_whether_it_removed_or_only_counted() {
    assert_eq!(
        cleaned_line("r1", 1, 3 * 1024 * 1024, true),
        "r1: would remove 1 cache entry (3 MB)"
    );
    assert_eq!(
        cleaned_line("r1", 2, 0, false),
        "r1: removed 2 cache entries (0 MB)"
    );
}
