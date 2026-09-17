//! The Twalk Sensor: a Matrix client that decrypts portal-room events,
//! enriches them with contact and consent context, and publishes them as
//! typed CloudEvents on the bus.
//!
//! Implementation starts with ticket 02 (walking skeleton). This crate
//! exists so the integration-test harness (ticket 01) has a package to
//! live in.
