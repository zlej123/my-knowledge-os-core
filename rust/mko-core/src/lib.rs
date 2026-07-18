#![forbid(unsafe_code)]

pub mod error;
pub mod front_matter;
pub mod model;
pub mod revision;
pub mod safe_yaml;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
