//! Comment-preserving editing of the TOML configuration files.

mod change;
mod document;
mod target;
mod validate;
mod value;

pub use change::{backup_failure, needs_rights, plan, write, write_with_counter, Change};
pub use document::ConfigDoc;
pub use target::{resolve, resolve_with, Target};
pub use validate::validate;
pub use value::parse_value;
