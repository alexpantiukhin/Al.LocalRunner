//! Comment-preserving editing of the TOML configuration files.

mod change;
mod document;
mod elevate;
mod target;
mod validate;
mod value;

pub use change::{backup_failure, needs_rights, plan, write, write_with_counter, Change};
pub use document::ConfigDoc;
#[cfg(windows)]
pub use elevate::OutputChannel;
pub use elevate::{
    attach_elevated_output, channel_pipes, child_args, elevated_command_line, new_channel,
    rerun_elevated, sudo_route, Elevator, OsElevator, SudoRoute, ELEVATED_CHILD,
};
pub use target::{resolve, resolve_with, Target};
pub use validate::validate;
pub use value::parse_value;
