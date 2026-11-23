//! Comment-preserving editing of the TOML configuration files.

mod change;
mod document;
mod elevate;
mod target;
mod validate;
mod value;

pub use change::{backup_failure, needs_rights, plan, write, write_with_counter, Change};
pub use document::ConfigDoc;
pub use elevate::{
    child_args, elevated_command_line, encoded_command, rerun_elevated, windows_child_script,
    Elevator, OsElevator, ELEVATED_CHILD,
};
pub use target::{resolve, resolve_with, Target};
pub use validate::validate;
pub use value::parse_value;
