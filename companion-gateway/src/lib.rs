//! The Twalk Companion Gateway: the backend serving the Companion PWA on its
//! own origin. Ticket #48 built the service skeleton — environment
//! configuration, the Companion's static files, a health endpoint, metrics,
//! structured logs with `traceparent` propagation, and a graceful shutdown;
//! ticket #52 added the user's session on top of it — sign-in through a
//! Matrix OpenID token ([`matrix_openid`]), the owner check and the
//! per-device tokens every other endpoint requires ([`session`],
//! [`session_http`]); ticket #49 added the consent store — an append-only
//! decision journal with the current state as its projection ([`store`]),
//! the write API over it ([`consent`], [`consent_http`]), and a
//! transactional outbox that publishes each committed decision exactly once
//! as a `consent.state.changed.v1` ([`outbox`]); ticket #53 added bootstrap —
//! the registration relay for the one and only account and the Sensor's
//! invitation into the rooms the user chooses ([`bootstrap`],
//! [`bootstrap_http`]); ticket #55 added the bridge login provisioning
//! facade — the Gateway speaks each configured bridge's own provisioning API
//! with that bridge's secret, **holds the blocking step of a QR login
//! itself**, and exposes a pollable state the Companion reads ([`bridge`],
//! [`bridge_http`]), so a phone that sleeps mid-scan loses a poll and not
//! the login; ticket #50 added the consent snapshot ([`consent_snapshot`]) —
//! the whole current state with the JetStream sequence it reflects,
//! authenticated by a service token, which is how a consumer whose cache is
//! cold recovers consent and then follows the bus (ADR 0010); ticket #56
//! added the other half of the bridge facade — bridge *status*
//! ([`bridge_status`], [`bridge_status_http`]): the webhook each bridge
//! pushes its connection state to, verified against that bridge's own
//! `as_token`, a `whoami` reconciliation at startup, the mapping from
//! mautrix's state vocabulary to the contract's, and the transitions
//! published as `bridge.status.changed.v1` through #49's outbox, so the
//! Gateway is the single producer of that event; ticket #54 made the Gateway
//! a consumer as well as a producer — the pending-contact projection
//! ([`contacts`], [`contacts_http`]), a durable consumer on
//! `inbound.message.received` that keeps **only** a contact's Matrix ID, the
//! network it wrote on and its first and last sighting, so the Companion can
//! say how many decisions are waiting. That store's restraint is the
//! feature: no body, no display name, no `network_identifier`, enforced by
//! the shape of the types rather than by care.
//!
//! Ticket #24 made the Gateway the **approval authority** ([`approval`],
//! [`approval_http`]): `POST /api/approvals` turns one suggestion into a
//! `persona.reply.approved.v1` on the bus, and refuses it when the
//! suggestion has expired, when its trigger was never consented, or when the
//! sender's consent is no longer `granted` *at that moment* — which is a
//! read of this Gateway's own consent state rather than a question asked of
//! another process. Spec #19 had put this endpoint on the Hermes runtime,
//! with the runtime asking the Gateway over HTTP; ADR 0022 records why it
//! moved here instead, and the short version is that the single writer of
//! consent state should not have to phone anybody to know what it wrote.
//! The module has no outbox, on purpose: an approval publishes inside its
//! own request or it is refused, because a send held for later is a send
//! whose consent check has gone stale.
//!
//! Ticket #97 gave that act something to act on ([`suggestions`],
//! [`suggestions_http`]): `GET /api/suggestions` and `GET
//! /api/suggestions/{id}`, the listing #100's approval screen draws from.
//! It is a **projection of the bus** and not a second store — the suggestion
//! lives in the stream, and a Gateway that kept its own copy would disagree
//! with a replay with nobody able to say which was true. It reads the same
//! bounded window an approval's lookup does and puts that bound in the
//! answer, and it tells an expired suggestion, a missing one and an
//! already-approved one apart by three different answers. What it says about
//! the message being answered is that message's id and type, and nothing
//! else: an excerpt belongs to the author of the quoted message rather than
//! to whoever sent the event carrying it (#110), so this projection never
//! opens an inbound event at all.
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

pub mod approval;
pub mod approval_http;
pub mod bootstrap;
pub mod bootstrap_http;
pub mod bridge;
pub mod bridge_http;
pub mod bridge_status;
pub mod bridge_status_http;
pub mod config;
pub mod consent;
pub mod consent_http;
pub mod consent_snapshot;
pub mod contacts;
pub mod contacts_http;
pub mod http;
pub mod matrix_openid;
pub mod metrics;
pub mod openapi;
pub mod outbox;
pub mod session;
pub mod session_http;
pub mod static_files;
pub mod store;
pub mod suggestions;
pub mod suggestions_http;
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
