use lr_cli::commands::setup::default_bin_dir;
use lr_cli::paths::{
    default_root, service_ipc_dir, service_root, Layout, CONFIG_DIR_ENV, STATE_DIR_ENV,
};
use std::path::PathBuf;

fn os_table(base: &std::path::Path) {
    if cfg!(windows) {
        std::env::set_var("LOCALAPPDATA", base.join("local"));
        std::env::set_var("ProgramData", base.join("data"));
        assert_eq!(
            default_root(),
            Some(base.join("local").join("Al.LocalRunner"))
        );
        assert_eq!(
            default_bin_dir().unwrap(),
            base.join("local").join("Programs").join("Al.LocalRunner")
        );
        assert_eq!(service_root(), base.join("data").join("Al.LocalRunner"));
    } else {
        std::env::set_var("HOME", base);
        assert_eq!(
            default_root(),
            Some(base.join(".local/share/al-localrunner"))
        );
        assert_eq!(default_bin_dir().unwrap(), base.join(".local").join("bin"));
        if cfg!(target_os = "macos") {
            let service = PathBuf::from("/Library/Application Support/Al.LocalRunner");
            assert_eq!(service_root(), service);
            assert_eq!(service_ipc_dir(), service.join("ipc"));
        } else {
            assert_eq!(service_root(), PathBuf::from("/var/lib/al-localrunner"));
            assert_eq!(service_ipc_dir(), PathBuf::from("/run/al-localrunner"));
        }
    }
}

fn resolve_order(base: &std::path::Path) {
    let flag = base.join("flag");
    let env_state = base.join("env-state");
    let env_config = base.join("env-config");
    std::env::remove_var(STATE_DIR_ENV);
    std::env::remove_var(CONFIG_DIR_ENV);
    let default = Layout::resolve(None, None).unwrap();
    assert_eq!(Some(default.root.clone()), default_root());
    assert_eq!(
        default.config, default.root,
        "config defaults to the state directory"
    );
    std::env::set_var(STATE_DIR_ENV, &env_state);
    std::env::set_var(CONFIG_DIR_ENV, &env_config);
    let env = Layout::resolve(None, None).unwrap();
    assert_eq!(
        (env.root, env.config),
        (env_state.clone(), env_config.clone())
    );
    let flags = Layout::resolve(Some(flag.clone()), Some(flag.join("cfg"))).unwrap();
    assert_eq!((flags.root, flags.config), (flag.clone(), flag.join("cfg")));
    let state_flag = Layout::resolve(Some(flag.clone()), None).unwrap();
    assert_eq!(
        (state_flag.root, state_flag.config),
        (flag.clone(), env_config.clone()),
        "--state-dir does not hide LOCALRUNNER_CONFIG_DIR"
    );
    std::env::set_var(STATE_DIR_ENV, "");
    std::env::set_var(CONFIG_DIR_ENV, "");
    let empty = Layout::resolve(None, None).unwrap();
    assert_eq!(
        Some(empty.root),
        default_root(),
        "an empty variable is unset"
    );
    std::env::remove_var(STATE_DIR_ENV);
    std::env::remove_var(CONFIG_DIR_ENV);
}

fn ipc_names(base: &std::path::Path) {
    let default = Layout::new(default_root().unwrap(), base.to_path_buf());
    let other = Layout::new(base.join("other"), base.join("other"));
    if cfg!(windows) {
        let sid = "S-1-5-21-1-2-3-1001";
        assert_eq!(
            default.hub_ipc(sid),
            format!(r"\\.\pipe\al-localrunner-hub-{sid}")
        );
        assert_eq!(
            default.runner_ipc(sid, "r1"),
            format!(r"\\.\pipe\al-localrunner-runner-{sid}-r1")
        );
        assert_ne!(default.hub_ipc(sid), default.hub_ipc("S-1-5-21-1-2-3-1002"));
        assert!(
            other
                .hub_ipc(sid)
                .starts_with(&format!(r"\\.\pipe\al-localrunner-hub-{sid}-")),
            "{}",
            other.hub_ipc(sid)
        );
        assert_ne!(other.hub_ipc(sid), default.hub_ipc(sid));
        assert_eq!(default.broker_ipc(), r"\\.\pipe\al-localrunner-broker");
        assert_eq!(default.admin_ipc(), r"\\.\pipe\al-localrunner-broker-admin");
    } else {
        let root = base.join("other");
        assert_eq!(
            PathBuf::from(other.hub_ipc("1000")),
            root.join("ipc").join("hub-1000.sock")
        );
        assert_eq!(
            PathBuf::from(other.runner_ipc("1000", "r1")),
            root.join("ipc").join("runner-1000-r1.sock")
        );
        let service = Layout::service();
        assert_eq!(
            PathBuf::from(service.broker_ipc()),
            service_ipc_dir().join("broker.sock")
        );
    }
}

#[test]
fn default_and_service_roots_follow_the_os_table_and_flags_beat_the_environment() {
    let base = std::env::temp_dir().join(format!("lr-paths-{}", std::process::id()));
    os_table(&base);
    resolve_order(&base);
    ipc_names(&base);
}
