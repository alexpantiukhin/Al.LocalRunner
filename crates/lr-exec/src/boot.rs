#![cfg_attr(not(unix), allow(dead_code))]

pub(crate) const TICK_BITS: u32 = 40;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Boot {
    Tag(u64),
    Since(u64),
    Unknown,
}

#[allow(dead_code)]
pub(crate) fn tag_of(boot_id: Option<&str>) -> Option<u64> {
    let id = boot_id?.trim();
    if id.is_empty() {
        return None;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    Some(hash >> TICK_BITS)
}

pub(crate) fn same_boot(start: u64, boot: Boot) -> bool {
    match boot {
        Boot::Tag(tag) => start >> TICK_BITS == tag,
        Boot::Since(at) => start >= at,
        Boot::Unknown => false,
    }
}

pub(crate) fn may_kill_left_group(start: u64, boot: Boot, in_scope: impl FnOnce() -> bool) -> bool {
    same_boot(start, boot) && !in_scope()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_of_this_boot_leaves_a_group_that_may_be_killed() {
        let start = (7 << TICK_BITS) | 12_345;
        assert!(may_kill_left_group(start, Boot::Tag(7), || false));
        assert!(may_kill_left_group(2_000, Boot::Since(1_000), || false));
    }

    #[test]
    fn a_marker_from_another_boot_leaves_the_group_alone() {
        let start = (7 << TICK_BITS) | 12_345;
        assert!(!may_kill_left_group(start, Boot::Tag(8), || false));
        assert!(!may_kill_left_group(500, Boot::Since(1_000), || false));
        assert!(!may_kill_left_group(start, Boot::Unknown, || false));
    }

    #[test]
    fn a_tree_in_a_cgroup_scope_is_left_to_the_scope_sweep() {
        let start = (7 << TICK_BITS) | 12_345;
        assert!(!may_kill_left_group(start, Boot::Tag(7), || true));
    }

    #[test]
    fn the_scope_is_not_probed_for_a_marker_of_another_boot() {
        let start = (7 << TICK_BITS) | 12_345;
        let probed = std::cell::Cell::new(false);
        assert!(!may_kill_left_group(start, Boot::Tag(8), || {
            probed.set(true);
            false
        }));
        assert!(!probed.get());
    }

    #[test]
    fn an_unreadable_or_empty_boot_id_gives_no_tag() {
        assert_eq!(tag_of(None), None);
        assert_eq!(tag_of(Some(" \n")), None);
        let a = tag_of(Some("6f1c0e9a-1\n")).unwrap();
        assert_eq!(Some(a), tag_of(Some("6f1c0e9a-1")));
        assert_ne!(Some(a), tag_of(Some("6f1c0e9a-2")));
        assert!(a < 1 << (64 - TICK_BITS));
    }
}
