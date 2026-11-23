//! Comment-preserving editing of the TOML configuration files.

mod document;
mod target;
mod validate;
mod value;

pub use document::ConfigDoc;
pub use target::{resolve, resolve_with, Target};
pub use validate::validate;
pub use value::parse_value;
