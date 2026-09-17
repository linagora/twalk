//! The Twalk Companion Gateway: the backend serving the Companion PWA on its
//! own origin. Ticket #48 built the service skeleton — environment
//! configuration, the Companion's static files, a health endpoint, metrics,
//! structured logs with `traceparent` propagation, and a graceful shutdown;
//! ticket #52 added the user's session on top of it — sign-in through a
//! Matrix OpenID token ([`matrix_openid`]), the owner check and the
//! per-device tokens every other endpoint requires ([`session`],
//! [`session_http`]); ticket #53 added bootstrap — the registration relay for
//! the one and only account and the Sensor's invitation into the rooms the
//! user chooses ([`bootstrap`], [`bootstrap_http`]). Consent and the bridge
//! facade land the same way in the remaining tickets of spec #46.
//!
//! Ticket #63 wrote that surface down: `companion-gateway/openapi.yaml` is
//! an OpenAPI 3.1 description of every answer the origin gives, served by
//! the origin itself ([`openapi`]) and checked against the running binary by
//! `tests/openapi.rs`. It is what the Companion generates its TypeScript
//! client from, so every ticket that adds an endpoint extends it in the same
//! commit — the test refuses a route that is not described.
//!
//! As in the Sensor, the seam-independent logic lives in these modules and
//! the binary in `main.rs` only wires them to the network.

pub mod bootstrap;
pub mod bootstrap_http;
pub mod config;
pub mod http;
pub mod matrix_openid;
pub mod metrics;
pub mod openapi;
pub mod session;
pub mod session_http;
pub mod static_files;
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
