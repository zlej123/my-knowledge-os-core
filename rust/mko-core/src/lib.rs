#![forbid(unsafe_code)]

pub mod atomic;
pub mod config;
pub mod error;
pub mod fingerprint;
pub mod front_matter;
pub mod model;
pub mod path_policy;
pub mod registry;
pub mod revision;
pub mod safe_yaml;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
