//! The Twalk Companion Gateway: the backend serving the Companion PWA on its
//! own origin. This ticket (#48) is the service skeleton — environment
//! configuration, the Companion's static files, a health endpoint, metrics,
//! structured logs with `traceparent` propagation, and a graceful shutdown.
//! Consent, authentication and the bridge facade land on top of it in the
//! later tickets of spec #46.
//!
//! As in the Sensor, the seam-independent logic lives in these modules and
//! the binary in `main.rs` only wires them to the network.

pub mod config;
pub mod http;
pub mod metrics;
pub mod trace;

/// The Gateway's version, as the health endpoint reports it: the package
/// version. This is the stable half of the version handshake — the Companion
/// is installed as a PWA, so a service worker may hold an app shell built
/// against an older Gateway; the client compares this value with the one
/// baked into its own build and reloads when they differ.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The revision this binary was built from (`git describe`, or whatever
/// `TWALK_BUILD_REVISION` named at build time, or `unknown` — see
/// `build.rs`). Provenance for an operator reading the health document, not
/// an input to the handshake.
pub const REVISION: &str = env!("TWALK_BUILD_REVISION");
