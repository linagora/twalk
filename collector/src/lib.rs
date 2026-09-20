//! The Twalk collector (ADR 0033, issue #274): the process that holds one
//! OIDC grant for the owner's own accounts — a mailbox, a calendar — and
//! publishes what involves other people as typed CloudEvents on the bus.
//!
//! One process per **grant**, not per connection: the client's refresh token
//! rotates on every renewal, so two processes sharing it would each hand the
//! SSO a token the other had already spent, and cut each other off. The mail
//! connection and the calendar connection of one SSO account are two
//! connections in the registry (ADR 0033) and one process here.
//!
//! The pure, seam-independent logic lives in these modules; the binary in
//! `main.rs` wires them to the SSO, the services and NATS.

pub mod config;
pub mod metrics;
pub mod oidc;
pub mod status;
