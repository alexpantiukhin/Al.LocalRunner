#[path = "../common/mod.rs"]
mod common;
mod failures;
mod helper;
mod services;
mod single_machine;
mod support;
mod two_machines;

#[test]
fn helper_process() {
    helper::run_if_requested();
}
