//! Comment-preserving editing of the TOML configuration files.

mod apply;
mod change;
mod document;
mod elevate;
mod target;
mod validate;
mod value;

pub use apply::{
    after_write, explicit_dirs, restart_line, restart_only_changes, restart_plan, restart_process,
    roles_of, run_change, run_change_with, ApplyOpts, LiveProcesses, LiveRoles, Phase,
    RestartOutcome, RestartPlan, RoleControl, RoleProcesses, TargetKind, EXIT_WRITTEN_STEP_FAILED,
    RESTART_ONLY,
};
pub use change::{backup_failure, needs_rights, plan, write, write_with_counter, Change};
pub use document::ConfigDoc;
#[cfg(windows)]
pub use elevate::OutputChannel;
pub use elevate::{
    attach_elevated_output, channel_pipes, child_args, command_words, elevated_command_line,
    new_channel, rerun_elevated, sudo_probe_args, sudo_route, untrusted_text, Elevator, OsElevator,
    SudoProbe, SudoRoute, ELEVATED_CHILD,
};
pub use target::{resolve, resolve_with, Target};
pub use validate::validate;
pub use value::parse_value;
