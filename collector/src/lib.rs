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

pub mod caldav;
pub mod calendars;
pub mod config;
pub mod consent;
pub mod freebusy;
pub mod fs;
pub mod http;
pub mod jmap;
pub mod mails;
pub mod metrics;
pub mod oidc;
pub mod outbound;
pub mod push;
pub mod replies;
pub mod side;
pub mod status;
/// Windows time-zone names and the IANA zone each one means: CLDR's table,
/// generated (see `collector/tools/generate-windows-zones.py`).
mod windows_zones;
pub mod zones;
