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
//! Ticket #98 gave the deployment its model and its language
//! ([`settings`], [`settings_http`]): the OpenAI-compatible endpoint, the
//! model name, the provider passthrough, the endpoint credential and the
//! user's native language, held here because an operator must be able to
//! change a model without shell access and because the Hermes runtime
//! injects the result into each persona rather than letting a persona fetch
//! it — the token that opens this configuration is the token that opens the
//! consent snapshot (ADR 0015, ADR 0016). Three decisions in it are the ones
//! to argue with. The credential is **write-only**: it goes in and the only
//! read that returns it is the runtime's, behind the service token, while a
//! browser is told which source is in force and the last four characters —
//! and a credential the operator supplied as a file **wins** over one set
//! from the browser, which is not a theoretical precedence but how the
//! reference deployment runs. The provider passthrough stays and is
//! documented as the escape hatch rather than the norm, because with an
//! OpenAI-compatible proxy in front every provider quirk lives in the
//! proxy's own configuration. And `POST /api/settings/model/probe` exists so
//! that an endpoint that cannot be reached, one that refused the request,
//! one answering something that is not a chat completion and no endpoint
//! configured at all are **four answers** rather than one silence — the
//! conflation behind nine incidents in two days.
//!
//! Ticket #105 added the portal register ([`portals`], [`portals_http`],
//! ADR 0024), which answers the defect that made a freshly connected network
//! publish nothing at all. A bridge builds a portal room lazily, as each
//! conversation becomes active, and invites only the user; nothing invited
//! the Sensor, so a working deployment sat outside seventeen of its
//! eighteen WhatsApp conversations while every component reported itself
//! healthy. The register reads each bridge's portal rooms **as that bridge's
//! own bot**, through the appservice token ticket #56 already put in
//! configuration — the first thing on this origin that *acts* with that
//! token rather than merely verifying a push with it — and it states, as a
//! number the deployment can say out loud, how many conversations the Sensor
//! is outside. What it deliberately does not do is invite the Sensor into
//! them: the mechanism is the Gateway's and the policy is the user's, one
//! conversation at a time (#143), because those eighteen rooms held some
//! 1,300 memberships and observing all of them by default is the decision
//! #122 is about. It keeps no store: the answer to "is this conversation
//! observed?" is the Sensor's own membership event, asked of the homeserver
//! on every read.
//!
//! Ticket #149 made the invariant `CONTEXT.md` states plainly true here too
//! ([`owner`]): the owner is never a contact and never has a consent state, on
//! any event (ADR 0018, ADR 0021). #109 and #147 stopped the Sensor
//! *producing* events that said otherwise; neither could reach the row a
//! deployment upgraded across #109 still holds, because before ADR 0018 the
//! user's own messages were published as a contact's and fed the
//! pending-contact projection. So this Gateway now knows which Matrix IDs are
//! the owner's (`GATEWAY_OWNER_IDENTITIES`, beside the `GATEWAY_OWNER` it
//! always contains — a set that is configured because the mautrix
//! provisioning API exposes no ghost Matrix ID at all, and that grows when a
//! network starts using a new addressing scheme), refuses a decision about one
//! of them with a code of its own, and serves none of them from any read of
//! its store — including a row recorded before this landed. The refusal is
//! **at the writer** ([`outbox::Outbox::record`]) and the exclusions are **in
//! the store's own SQL** ([`store`]), which is where the `persona` exclusion
//! already lives: an endpoint added later cannot forget either. Nothing is
//! deleted — the journal is append-only by design and is the audit trail of a
//! confidentiality promise — so the rows stay, withheld, counted on
//! `/metrics` and named in a startup warning. And because the set cannot be
//! derived, the Gateway **serves** it on the snapshot it already serves the
//! Sensor, so that the second component needing it reads the single writer's
//! list instead of maintaining its own.
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
pub mod hermes_answer;
pub mod hermes_answer_http;
pub mod http;
pub mod matrix_openid;
pub mod metrics;
pub mod openapi;
pub mod outbox;
pub mod owner;
pub mod portals;
pub mod portals_http;
pub mod runtime_presence;
pub mod runtime_presence_http;
pub mod session;
pub mod session_http;
pub mod settings;
pub mod settings_http;
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
