#![forbid(unsafe_code)]

//! Shared Telegram channel primitives.
//!
//! `capture` contains pure, bounded Update normalization. A future `delivery`
//! module may share only wire/identity primitives; it must not share Capture's
//! routing authorization or intake state.

pub mod binding;
pub mod capture;
pub mod connection;
pub mod pairing;
pub mod secret;
pub mod transport;
