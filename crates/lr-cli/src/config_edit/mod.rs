//! Comment-preserving editing of the TOML configuration files.

mod apply;
mod change;
mod document;
mod elevate;
mod target;
mod validate;
mod value;

pub use apply::{
    restart_only_changes, run_change, run_change_with, ApplyOpts, LiveRoles, RoleControl,
    TargetKind, RESTART_ONLY,
};
pub use change::{backup_failure, needs_rights, plan, write, write_with_counter, Change};
pub use document::ConfigDoc;
#[cfg(windows)]
pub use elevate::OutputChannel;
pub use elevate::{
    attach_elevated_output, channel_pipes, child_args, elevated_command_line, new_channel,
    rerun_elevated, sudo_probe_args, sudo_route, untrusted_text, Elevator, OsElevator, SudoProbe,
    SudoRoute, ELEVATED_CHILD,
};
pub use target::{resolve, resolve_with, Target};
pub use validate::validate;
pub use value::parse_value;
