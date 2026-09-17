//! The Twalk Sensor: a Matrix client that decrypts portal-room events,
//! enriches them with contact and consent context, and publishes them as
//! typed CloudEvents on the bus.
//!
//! The pure, seam-independent logic lives in these modules; the binary in
//! `main.rs` only wires them to matrix-sdk and NATS.

pub mod config;
pub mod consent;
pub mod network;
pub mod normalize;
