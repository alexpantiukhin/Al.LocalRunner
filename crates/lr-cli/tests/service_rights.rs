mod common;

use common::TempDir;
use lr_cli::commands::service_cmd::{autostart, install_service};
use lr_cli::commands::{RoleArg, Switch};
use lr_cli::os_service::{service_installed, Os};
use lr_cli::paths::Layout;

#[test]
fn broker_service_commands_without_administrator_rights_are_refused_before_any_change() {
    if lr_cli::host::is_elevated() {
        eprintln!("SKIPPED: the test runs elevated, the refusal without rights cannot be seen");
        return;
    }
    let dir = TempDir::new("no-admin");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    let installed = service_installed(Os::current());
    assert_eq!(
        install_service(&layout, false),
        Err(
            "broker install-service needs administrator rights (run it elevated or with sudo)"
                .to_string()
        )
    );
    assert_eq!(
        autostart(&layout, Switch::Off, Some(RoleArg::Broker), None, false),
        Err(
            "autostart off broker needs administrator rights (run it elevated or with sudo)"
                .to_string()
        )
    );
    assert_eq!(
        autostart(&layout, Switch::On, Some(RoleArg::Broker), None, false),
        Err(
            "broker install-service needs administrator rights (run it elevated or with sudo)"
                .to_string()
        )
    );
    assert_eq!(service_installed(Os::current()), installed);
    assert!(
        !dir.path().join("state").exists() && !dir.path().join("config").exists(),
        "nothing was written"
    );
}
