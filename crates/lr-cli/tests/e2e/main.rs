#[path = "../common/mod.rs"]
mod common;
mod dashboard_events;
mod failures;
mod helper;
mod kit;
mod moved_broker;
mod services;
mod single_machine;
mod support;
mod two_machines;

#[test]
fn helper_process() {
    helper::run_if_requested();
}
