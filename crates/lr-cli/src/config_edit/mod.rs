//! Comment-preserving editing of the TOML configuration files.

mod apply;
mod change;
mod document;
mod elevate;
mod records;
mod target;
mod validate;
mod value;

pub use apply::{
    after_write, explicit_dirs, readers, restart_line, restart_only_changes, restart_plan,
    restart_process, restart_service, run_change, run_change_with, ApplyOpts, LiveProcesses,
    LiveRoles, Phase, Readers, RestartOutcome, RestartPlan, RoleControl, RoleProcesses, TargetKind,
    EXIT_FILE_CHANGED, EXIT_WRITTEN_STEP_FAILED, EXPECT_BEFORE, RESTART_ONLY,
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
pub use records::{
    add_local_user, add_name, add_shared, remove_local_user, remove_name, remove_shared,
};
pub use target::{resolve, resolve_with, Target};
pub use validate::validate;
pub use value::parse_value;
