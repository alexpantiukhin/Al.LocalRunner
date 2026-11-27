use lr_core::{VariantWhen, Zone};

fn when(os: &[&str], zones: &[Zone], tags: &[&str]) -> VariantWhen {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    VariantWhen {
        zones: zones.to_vec(),
        os: s(os),
        tags: s(tags),
        ..Default::default()
    }
}

#[test]
fn a_filter_is_within_another_when_everything_it_matches_the_other_matches() {
    let everything = VariantWhen::default();
    let linux = when(&["linux"], &[], &[]);
    let linux_remote = when(&["linux"], &[Zone::Remote], &[]);
    assert!(linux.is_within(&everything));
    assert!(linux_remote.is_within(&linux));
    assert!(!linux.is_within(&linux_remote));
    assert!(!everything.is_within(&linux));
    assert!(when(&["linux"], &[], &[]).is_within(&when(&["linux", "macos"], &[], &[])));
}

#[test]
fn two_filters_overlap_when_some_runner_can_match_both() {
    let linux = when(&["linux"], &[], &[]);
    let windows = when(&["windows"], &[], &[]);
    let remote = when(&[], &[Zone::Remote], &[]);
    assert!(!linux.overlaps(&windows));
    assert!(linux.overlaps(&remote));
    assert!(linux.overlaps(&linux));
    assert!(!when(&[], &[Zone::Remote], &[]).overlaps(&when(&[], &[Zone::Lan], &[])));
}
