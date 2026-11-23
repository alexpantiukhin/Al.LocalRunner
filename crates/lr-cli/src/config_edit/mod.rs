//! Comment-preserving editing of the TOML configuration files.

mod document;
mod value;

pub use document::ConfigDoc;
pub use value::parse_value;
