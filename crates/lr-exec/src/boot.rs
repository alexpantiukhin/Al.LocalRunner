#![cfg_attr(not(unix), allow(dead_code))]

pub(crate) const TICK_BITS: u32 = 40;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Boot {
    Tag(u64),
    Since(u64),
    Unknown,
}

pub(crate) fn same_boot(start: u64, boot: Boot) -> bool {
    match boot {
        Boot::Tag(tag) => start >> TICK_BITS == tag,
        Boot::Since(at) => start >= at,
        Boot::Unknown => false,
    }
}

pub(crate) fn may_kill_left_group(start: u64, boot: Boot, in_scope: bool) -> bool {
    !in_scope && same_boot(start, boot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_of_this_boot_leaves_a_group_that_may_be_killed() {
        let start = (7 << TICK_BITS) | 12_345;
        assert!(may_kill_left_group(start, Boot::Tag(7), false));
        assert!(may_kill_left_group(2_000, Boot::Since(1_000), false));
    }

    #[test]
    fn a_marker_from_another_boot_leaves_the_group_alone() {
        let start = (7 << TICK_BITS) | 12_345;
        assert!(!may_kill_left_group(start, Boot::Tag(8), false));
        assert!(!may_kill_left_group(500, Boot::Since(1_000), false));
        assert!(!may_kill_left_group(start, Boot::Unknown, false));
    }

    #[test]
    fn a_tree_in_a_cgroup_scope_is_left_to_the_scope_sweep() {
        let start = (7 << TICK_BITS) | 12_345;
        assert!(!may_kill_left_group(start, Boot::Tag(7), true));
    }
}
