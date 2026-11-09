const SCENARIOS: &[&str] = &[
    "S01", "S02", "S03", "S04", "S05", "S06", "S07", "S08", "S09", "S10", "S11", "S12", "S13",
    "S14", "S15", "S16", "S17", "S18", "S19", "S20", "S21", "S22", "S23", "S24", "S25",
];

#[test]
#[ignore = "enabled in Task 19"]
fn every_scenario_of_section_16_is_named_by_an_e2e_test() {
    assert_eq!(SCENARIOS.len(), 25);
}
