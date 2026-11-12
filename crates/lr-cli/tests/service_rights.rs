mod common;

use common::TempDir;
use lr_cli::commands::service_cmd::{
    autostart, autostart_with, install_service, install_service_with,
};
use lr_cli::commands::{RoleArg, Switch};
use lr_cli::os_service::{service_installed, Os};
use lr_cli::paths::Layout;

const INSTALL: &str =
    "broker install-service needs administrator rights (run it elevated or with sudo)";
const OFF: &str = "autostart off broker needs administrator rights (run it elevated or with sudo)";

#[test]
fn broker_service_commands_without_administrator_rights_are_refused_before_any_change() {
    let dir = TempDir::new("no-admin");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    let installed = service_installed(Os::current());
    assert_eq!(
        install_service_with(&layout, false, false),
        Err(INSTALL.to_string())
    );
    assert_eq!(
        autostart_with(
            &layout,
            Switch::Off,
            Some(RoleArg::Broker),
            None,
            false,
            false
        ),
        Err(OFF.to_string())
    );
    assert_eq!(
        autostart_with(
            &layout,
            Switch::On,
            Some(RoleArg::Broker),
            None,
            false,
            false
        ),
        Err(INSTALL.to_string())
    );
    assert_eq!(service_installed(Os::current()), installed);
    assert!(
        !dir.path().join("state").exists() && !dir.path().join("config").exists(),
        "nothing was written"
    );
}

#[test]
fn the_commands_read_the_rights_of_this_process() {
    if lr_cli::host::is_elevated() {
        eprintln!(
            "SKIPPED: the process is elevated; running the real commands here would install \
             or remove the broker service"
        );
        return;
    }
    let dir = TempDir::new("no-admin-real");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    assert_eq!(install_service(&layout, false), Err(INSTALL.to_string()));
    assert_eq!(
        autostart(&layout, Switch::Off, Some(RoleArg::Broker), None, false),
        Err(OFF.to_string())
    );
}
