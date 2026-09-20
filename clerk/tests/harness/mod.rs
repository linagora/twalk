//! Integration-test harness for the clerk (ticket #265).
//!
//! What every component's suite needs — the shared test stack's lifecycle
//! (Synapse and the bus), the `Bus`, contract validation, `poll_until` —
//! lives in the shared harness crate (`tests/harness/`, ticket #20) and is
//! re-exported here, so the clerk's test files see one flat `harness::`
//! namespace, the way the Sensor's and Hermes's do.
//!
//! What is the clerk's own is the seam's other side: a **real Buzz relay**
//! ([`RelayStack`], from `clerk/tests/compose.relay.yaml`), seeded by signed
//! events the way an operator seeds the owner's relay, and the clerk binary
//! itself at its process boundary ([`ClerkProc`]). Nothing here reaches
//! inside the clerk: a test publishes on the bus, reads the relay back as
//! the owner, and reads the clerk's own `/metrics` and logs.
//!
//! Isolation: the relay stack persists across runs, so every channel a
//! test creates is fresh (a new UUID), every clerk key is fresh, and every
//! run has a bus stream and subject prefix of its own — the same shape as
//! the Hermes suite. The relay stack itself goes with
//! `docker compose -p twalk-clerk-test -f clerk/tests/compose.relay.yaml down -v`
//! (`TWALK_CLERK_TEST_STACK` names the project when it is not the default).

// Every test binary compiles this module but uses only a subset of it.
#![allow(dead_code, unused_imports)]

/// The clerk binary at its process boundary: [`ClerkProc`].
mod clerk;

/// The real Buzz relay and the signed client that seeds and reads it:
/// [`RelayStack`], [`Channels`], [`relay_env`], [`fresh_clerk_key`].
mod relay;

pub use clerk::*;
pub use relay::*;
pub use twalk_test_harness::*;

use std::time::{SystemTime, UNIX_EPOCH};

/// An identifier unique to this run of this test: the relay stack and the
/// bus both persist across runs, so every name a test claims on either —
/// the stream, the subject prefix, a channel's name — carries one.
pub fn run_id(test_name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    format!(
        "c265-{test_name}-{}-{}",
        std::process::id(),
        nanos % 1_000_000
    )
}
