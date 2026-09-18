# companion-gateway

The Companion backend (Rust): bridge provisioning facade, persona orchestrator, and consent broker. It is the single writer of consent state (see `docs/architecture/adr/0006-consent-state-owned-by-companion-gateway.md`).

What exists today is the service skeleton (ticket #48) — the origin that serves the Companion, a health endpoint, metrics, structured logs and a graceful shutdown — the user's session (ticket #52): sign-in with a Matrix OpenID token, the owner check, and per-device tokens with revocation — the consent store (ticket #49): an append-only decision journal in SQLite, the current state as its projection, and a transactional outbox that publishes each committed decision exactly once as a `consent.state.changed.v1` — bootstrap (ticket #53): the registration relay that creates this deployment's one account, and the Sensor's invitation into the rooms the user chooses — the consent snapshot (ticket #50): the whole current state with the bus sequence it reflects, served to a consumer whose cache is cold — the bridge login facade (ticket #55): each configured bridge's provisioning API driven by the Gateway, which holds the blocking step of a QR login itself — and the pending-contact projection (ticket #54), which makes the Gateway a consumer of the bus as well as its producer: a durable consumer on `inbound.message.received` that keeps a contact's Matrix ID, its network and its first and last sighting, and nothing else. What is left of specs #46 and #47 lands on top of them.

## The origin

One HTTP origin serves everything, so there is no CORS and the device token can later be an `HttpOnly` cookie (ADR 0011):

| Path | Answer |
| --- | --- |
| `/health` | `200` with `{"status":"ok","version":"…","revision":"…"}`. `version` is the server half of the Companion's version handshake: the PWA compares it with the version baked into its own build and reloads when a service worker has left it holding a stale app shell. |
| `/metrics` | The Prometheus text exposition, in the Sensor's conventions (`twalk_companion_gateway_*`). |
| `/openapi.yaml` | This origin's own OpenAPI 3.1 description — the bytes of `openapi.yaml`, embedded in the binary. See below. |
| `/api/session` | `POST` signs in, `GET` reports who is signed in on which device, `DELETE` signs this device out (which revokes it). |
| `/api/session/refresh` | `POST` exchanges the refresh cookie for a new pair of tokens, rotating both. |
| `/api/devices` | `GET` lists the devices: name, created, last seen, revoked. |
| `/api/devices/{id}` | `DELETE` revokes one device. Its token stops working on its next request. |
| `/api/bootstrap/account` | `POST` creates this deployment's one account, once (screen 2). Open, because there is no account to sign in with yet. |
| `/api/bootstrap/rooms` | `POST` invites the Sensor into the rooms the user selected (screen 3d), with the user's own Matrix access token, which is then forgotten. |
| `/api/consent/decisions` | `POST` records one consent decision (below). |
| `/api/consent/state` | `GET` returns the current state, one entry per (subject, network), as recorded. |
| `/api/consent/effective` | `GET` returns the state that applies to one contact on one network, with the precedence resolved. |
| `/api/consent/snapshot` | `GET` returns the whole state with the JetStream sequence it reflects, for a consumer starting cold (below). The one route that takes a **service token** and refuses a device token. |
| `/api/contacts/pending` | `GET` returns the contacts that have written and that no decision covers, with a count per network (below). |
| `/api/contacts/display-names` | `GET` returns what those contacts are called, read from the bus and stored nowhere. |
| `/api/…` (anything else) | A JSON error, never HTML: a client parsing a response must not be handed a page. Unauthenticated, that is a `401` — the guard answers before routing, so an unknown path tells a caller with no device token nothing about the API's shape. |
| anything else | The Companion's build in `GATEWAY_STATIC_DIR`, resolved the way SvelteKit's static adapter lays it out (see below). |

### Resolving a path to a file of the Companion's build

The Companion is a SvelteKit static export: a prerendered page is a file (`onboarding/whatsapp.html`, or `onboarding/signal/index.html` when the build keeps trailing slashes), and a route that was not prerendered exists only in the client-side router. So `src/static_files.rs` follows the adapter's own preview-server order: the exact file; else the path plus `index.html` (trailing slash) or plus `.html`; else a `307` to whichever trailing-slash spelling the build does have; else the SPA fallback, with `200`.

- The fallback is `200.html` (`GATEWAY_FALLBACK_FILE`), not `index.html` — the adapter's documentation warns that an `index.html` fallback collides with a prerendered homepage.
- Content types come from the path's extension and default to `text/html`. `.wasm` is exactly `application/wasm`, with no parameters: `WebAssembly.instantiateStreaming` rejects anything else with a `TypeError` and the Companion has no fallback path, so a stray `charset` would break onboarding with an error that looks nothing like a MIME problem.
- A pre-compressed sibling (`crypto.wasm.br`, `crypto.wasm.gz`) is served, with its `Content-Encoding`, to a client that accepts that encoding — the Matrix crypto module is ~7.5 MB raw against ~1.3 MB brotli-compressed.

### Sign-in, and what guards the rest

The Companion is a static export with no secret of its own, so the user's Matrix identity is the only identity source ([ADR 0011](../docs/architecture/adr/0011-gateway-authenticates-with-matrix-openid.md)). A sign-in is four steps, in `src/session.rs`:

1. **Verify.** The Companion posts the OpenID token it got from the homeserver (`POST /_matrix/client/v3/user/{userId}/openid/request_token`); the Gateway hands it back to that homeserver at `GET /_matrix/federation/v1/openid/userinfo`, which answers `{"sub": "@user:server"}`. That endpoint is the unauthenticated corner of the federation API — no federation identity, no signing keys, one outbound HTTP call — and the Gateway checks that the domain of `sub` is the server it asked, as the specification requires. The federation base URL is pinned in configuration instead of resolved from the server name: one deployment, one homeserver, and the full resolution algorithm would be a federation stack in a service that does not federate.
2. **Check the owner.** `GATEWAY_OWNER` names the single human this deployment serves. Every other Matrix ID is refused — the homeserver has other accounts, the Sensor's among them.
3. **Refuse a replay.** Synapse's verification does not consume the token: it answers the same for an hour. So the Gateway keeps its own ledger of accepted tokens — SHA-256 digests, never the tokens — and refuses a second sign-in with one it has already seen.
4. **Issue its own tokens.** A short-lived device token (`twalk_device`) and a long-lived refresh token (`twalk_refresh`, scoped to `/api/session`), both 256 random bits, both stored as digests, both belonging to one row of the device list. The cookies are `HttpOnly`, `SameSite=Lax`, and `Secure` unless the origin is localhost. Refreshing rotates both, so a device token that leaked dies at the next refresh rather than living out its lifetime.

No Matrix access token is ever stored or logged — asserted, not promised: `tests/signin.rs` signs in against a real Synapse and then reads the store's bytes and the process's captured log output looking for the tokens involved.

Authentication is the default and not an opt-in: everything under `/api/` requires a live device token unless `session_http::requirement` says otherwise, so a route added by a later ticket is protected before its author writes a line of authentication code. The exceptions are the sign-in itself, the refresh (which authenticates the refresh cookie and rotates it), and the consent snapshot, which takes a service token because the Sensor is not a device: it is one line in that table, and the guard asks for no device cookie there and injects no device identity, leaving `src/consent_snapshot.rs` to check the service token from its own configuration.

Revocation is immediate because nothing is cached: every authenticated request reads the device's row, so a revoked device is refused on its very next request. Revoked rows stay in the list, with the date, and their token digests are dropped.

With no `GATEWAY_OWNER` configured, nobody can sign in: the origin still serves the Companion, `/health` and `/metrics`, and every `/api` endpoint answers `503 sign_in_not_configured` naming the variable. That fails in the safe direction — nothing can be authenticated, so nothing can be decided — and keeps the page that can explain the problem, which refusing to start would take away.

### Bootstrap: one account, and the Sensor's invitation

Screen 2 promises a non-technical user an account; screen 3d promises to observe rooms of an existing Matrix account. Neither works without the Gateway: open self-service registration would turn a personal server into a public one, and the Sensor observes only the rooms it was invited to. `src/bootstrap.rs` does both, and refuses more.

**The registration relay.** `POST /api/bootstrap/account` takes a username and a password and creates the account with Synapse's admin registration endpoint (`POST /_synapse/admin/v1/register`), which is authenticated by a hex HMAC-SHA1 over the request's fields keyed with the registration shared secret — not by an admin token — and which works with `enable_registration: false`. That is the point: public registration stays closed while this one account can still be created.

Exactly one account, ever, enforced in two places that cannot both be lost:

- the username must be the localpart of `GATEWAY_OWNER`, checked before the registration secret is used at all — any other username is `403 not_the_owner`;
- once the account exists, every further attempt is `409 account_already_exists`. The store remembers the creation, in a table whose schema admits one row (`CHECK (id = 1)`), and the homeserver's own `M_USER_IN_USE` is honoured as the same refusal — so wiping the Gateway's volume does not re-open the window.

The endpoint is open, because it runs before any account exists and therefore before anyone can sign in. What that costs is stated plainly rather than hidden: until the owner finishes screen 2, whoever can reach the origin can claim the owner's account with a password of their choosing. So the relay is opt-in — no `GATEWAY_REGISTRATION_SHARED_SECRET`, no endpoint — and the window closes for good on the first success. An operator who provisions the account with `deploy/docker-compose/provision.sh` never opens it.

**The recovery key never arrives and never leaves.** The key is generated in the browser and used there ([ADR 0014](../docs/architecture/adr/0014-companion-crypto-runs-in-the-browser.md)), so screen 2's "Twalk never sees it" is a property of where the code runs. The API has no field one could ride in on (`deny_unknown_fields`), and a request carrying anything recovery-key-shaped is refused with `400 recovery_key_refused` rather than quietly ignored — a client with that bug should find out at once. The answer carries the Matrix session and nothing else: `user_id`, `device_id`, `access_token`, `home_server`. `tests/bootstrap.rs` asserts both halves, the response's field list included.

**The access token passes through.** The registration answer's token is what the browser bootstraps cross-signing with, so it is returned — and not kept: not in the store, not in a log line, not in the process beyond the response. The same holds for `POST /api/bootstrap/rooms`, which takes the user's Matrix access token as a parameter of one operation: the Gateway reads the Sensor's membership in each selected room, invites it where it is absent, and drops the token when the call returns. One room failing (`M_FORBIDDEN` in somebody else's room, a malformed id) is reported per room; a token the homeserver rejects fails the whole request, because then nothing was attempted anywhere. Asking twice is `already_present`, not an error.

Inviting with the user's own token rather than an admin credential is what makes this work on a homeserver with password login disabled: an invitation needs no more than the inviter's own session. And the Sensor's account stays provisioned by the compose stack — nothing here is on its startup path, which `tests/deployment.rs` asserts from compose's own resolved configuration.

A `traceparent` on an inbound request is continued (and returned on the response); a request without one, or with a malformed one, gets a fresh W3C trace context. Per-request log lines (method, path, status, duration, `traceparent`) are at `debug`; the lifecycle is at `info`.

### The HTTP description (ticket #63)

`openapi.yaml`, next to this README, is an OpenAPI 3.1 description of the whole origin: every endpoint, its authentication, its response shapes, and — per status — the machine-readable `error` code a client branches on. The Companion is built in its own lot, by another agent, from a TypeScript client generated against it. That is the property the CloudEvents contract gives the bus, applied to this origin: components built in parallel without coordination, and a field that changes breaks a build instead of an onboarding screen.

- **Where it lives.** With the component, not in `contracts/`. `contracts/` is the *bus* contract — CloudEvents schemas and fixtures, released CC0, shared by every component and consumed by third parties; this describes one component's own HTTP surface, moves with that component's code, and is embedded in its binary with `include_str!`. Putting it in `contracts/` would have widened what that directory means and coupled a crate's build to a path outside it.
- **Which way the check runs.** The description is the source of truth and the implementation is checked against it. `tests/openapi.rs` reads the file, drives the real binary through every operation it declares, and asserts the status, the media type, the body against the response's own JSON Schema (OpenAPI 3.1 schemas *are* JSON Schema 2020-12 — that is why 3.1), the `error` code, and the authentication each operation declares against `session_http::requirement` itself. It also asserts that every route the router registers is described and every described path is a route. Generating the description from the handlers would have described whatever they happen to do, mistakes included, and the Companion's lot would generate a client from a document nobody reviewed.
- **The obligation on later tickets.** Every ticket of spec #46 that adds an endpoint extends `openapi.yaml` in the same commit. It is not a convention to remember: an undescribed route fails the suite.

`GET /openapi.yaml` serves the committed file's bytes (`application/yaml`, RFC 9512), unauthenticated — a generator must be able to read it before anyone can sign in, and it holds no secret.
## Connecting a network: the bridge login facade (ticket #55)

A bridge's provisioning API lives on the bridge's own appservice listener (`/_matrix/provision/v3/*`), and its QR step **blocks**: the caller POSTs to it and the request stays open until the phone answers. A browser cannot hold that — a phone that locks mid-scan kills the request, and the login dies with it. So the Gateway holds it, and the Companion polls.

- **The Gateway holds the blocking step.** One task per bridge sits in `POST .../login/step/{process}/{step}/display_and_wait`. Each answer it gets — a refreshed QR, the next step, the completion — updates an in-memory document and bumps its `generation`.
- **The browser polls.** `GET /api/bridges/{bridge_id}/login` answers immediately, always: the current `state`, the current `step` with its payload and how long it is valid, and that `generation`. A refresh is a bumped generation with a new `step.payload.data` — the signal to redraw *before* the code on screen expires. `data` is the raw payload: a mautrix bridge renders no image, so the browser draws the code.
- **The browser never calls a bridge**, even though mautrix's CORS would allow it. The provisioning secret starts and destroys logins on the user's account (`docs/architecture/security-model.md`), so it stays on this side; the acting Matrix user is the configured owner, because mautrix's shared-secret auth takes that parameter on trust.
- **One login at a time per bridge.** The contract has no login dimension in v0.1 (spec #47). A second start is `409 login_in_flight`, naming the device and the instant that started the first — so the user can tell "my other phone is mid-scan" from "the server is stuck". A login that completed, failed or was cancelled does not stand in the way.
- **Reconnect is re-login.** `POST .../login` with `login_id` restarts the flow against the login the bridge already holds (mautrix's `?login_id=`). It repairs a broken session and never restarts a container.
- **A login in flight survives no restart**, of the bridge or of the Gateway: the process lives in the bridge's memory (capped at 30 minutes) and the held request lives in this one's. That is an error code, `login_lost`, not a silence.
- **A step the Companion cannot drive fails loudly.** `webauthn` (which WhatsApp can inject mid-flow) and `client_http` end the login with `webauthn_required` / `unsupported_step` and cancel the process, rather than hanging on a step nobody will answer. A bridge with `provisioning.fail_on_webauthn` refuses it one step earlier, which is the setting to prefer.
- **Credentials pass through and are never stored** (ADR 0011). A QR payload, a pairing code, the SMS preview path's Google cookies: relayed to the bridge, held in memory only until the next step replaces them, in no store and in no log line. What a log line records is the *shape* of what went through (`bridge::redacted`), and `BridgeConfig`'s `Debug` prints `<redacted>` for the secret.

```http
POST /api/bridges/mautrix-whatsapp/login      → 201, first step (a QR) in the body
GET  /api/bridges/mautrix-whatsapp/login      → poll: state, step, generation, expires_at
POST /api/bridges/mautrix-whatsapp/login/submit  → answer a user_input or cookies step
DELETE /api/bridges/mautrix-whatsapp/login    → cancel; 204
GET  /api/bridges/mautrix-whatsapp/logins     → what the bridge already holds (reconnect, logout)
DELETE /api/bridges/mautrix-whatsapp/logins/{login_id} → log out; 204
```

The whole surface, with every status and every error code, is in `openapi.yaml`.

**What the tests do not prove.** A real mautrix bridge needs a live WhatsApp or Signal account and a human with a phone, so it is never in the suite. `tests/bridges.rs` runs against a **stub bridge** implementing the provisioning contract (`tests/harness/stub_bridge.rs`), whose blocking step the test releases on command: a full login, a refresh mid-flow, a cancellation, the concurrent-login refusal, a login lost when the bridge restarts, and that no credential reaches the store or the logs. The facade's *network* side is therefore not proven by tests — an accepted limitation of spec #47, stated rather than discovered.

## Bridge health: the status webhook (ticket #56)

The other half of the facade, and the sole producer of `bridge.status.changed.v1` on the bus. Mautrix offers exactly **one** push channel — each bridge's `homeserver.status_endpoint` — and this is its other end.

- **The webhook is the channel.** A bridge POSTs its `BridgeState` to `POST /_twalk/bridges/{bridge_id}/status`. It is outside `/api/` because its caller is a bridge and not a browser, and it is the one route on this origin that a device token does not open.
- **The push is verified, never trusted.** The credential is that bridge's own `as_token` (`GATEWAY_BRIDGE_<ID>_AS_TOKEN`), as `Authorization: Bearer`. Trusting the compose network was explicitly refused: every container on it can reach this port, and a forged push could tell the user a dead session was healthy. A bridge the Gateway holds no token for is refused with `as_token_not_configured`, not accepted. The requirement is declared in the guard's own table (`session_http::requirement`, `Requirement::BridgeToken`), so the authentication policy stays one function with no hole in it.
- **Startup reconciles, it does not poll.** `GET /_matrix/provision/v3/whoami` runs once per bridge at startup, so a Gateway that restarts does not carry a stale `connected` forward. It is not a heartbeat: a bridge's state lives in the bridge's memory, is empty right after a bridge restart and carries no "last connected" field, so polling would miss every transition between two reads. A bridge that cannot be reached keeps its last known state and logs why — in a compose stack everything starts at once, and "I could not ask" must not become "it is down".
- **Management-room notices are not parsed.** The default `bridge_status_notices` setting suppresses a logout entirely, and what it does emit is human markdown.
- **The mapping table is the Gateway's** (`src/bridge_status.rs`):

  | mautrix `state_event` | contract state |
  | --- | --- |
  | `BAD_CREDENTIALS` | `session_expired` |
  | `TRANSIENT_DISCONNECT` | `degraded` |
  | `CONNECTING`, `BACKFILLING` | `starting` |
  | `CONNECTED` | `connected` |
  | `UNKNOWN_ERROR`, `LOGGED_OUT`, anything else | `disconnected` |

  The first row is the one the ticket turns on: a session revoked from the user's own phone reports **`BAD_CREDENTIALS`**, and no mautrix bridge emits `LOGGED_OUT` at all. The bridge's own `message` becomes `reason` (its `error` code when there is no message), its `timestamp` becomes `occurred_at`, and `last_message_at` is filled when a bridge puts one in `info` — none does today.
- **Only transitions are published.** The reported state is compared with the one the store holds; an identical state records nothing and publishes nothing, which is what a bridge's periodic re-push and its own retries are. A bridge nobody has heard from counts as `disconnected` — the honest prior, and what the first event's `from_state` says.
- **The same outbox as consent.** A transition is committed to `bridge_status_change` and published by #49's loop, with the contract's deterministic id as `Nats-Msg-Id`. One publisher, one set of exactly-once rules. The store is also what makes de-duplication survive a restart: `from_state` after a restart is the state the bridge was really in.
- **`bridge_id` is the contract's, not the instance's.** Events carry `^bridge-[a-z0-9-]+$` (`bridge-whatsapp`), which is also the segment of the webhook's URL; `/api/bridges` keeps speaking the instance id an operator configured (`mautrix-whatsapp`). The first is the identity third parties read off the bus, so it must be stable across restarts; the second names the software. `GATEWAY_BRIDGE_<ID>_STATUS_ID` sets it, and its default is the instance id with `mautrix-` stripped under a `bridge-` prefix — which is what the reference deployment already points each bridge at.

`tests/bridge_status.rs` drives all of it against the stub bridge: every mautrix state through the mapping table, a repeated state producing no second event, a startup reconciliation that publishes what `whoami` reports and one that agrees and says nothing, and a push refused with no token, with the wrong one, with a device cookie, and for a bridge with no token configured. A real mautrix bridge is not in the suite, for the same reason as above.

## Consent

The Gateway is the single writer of consent state, and its own store is the record of truth — the bus is the audit trail, not the memory (ADR 0010).

- **The journal is append-only.** Every decision is one row in `consent.sqlite3`, and SQLite triggers refuse any `DELETE` and any `UPDATE` of a recorded field. The only mutable column is the outbox's `published_at`.
- **The current state is a projection.** `consent_state` is a SQL *view* over the journal — the most recent decision per (subject, network) — so it cannot drift from the decisions it derives from, and no code path can write state without writing a decision.
- **Publication goes through a transactional outbox.** Commit, then publish, then mark published. A crash in between republishes rather than loses, and the event carries the contract's deterministic id as `Nats-Msg-Id`, so the bus deduplicates a republished row. A request never waits for the bus: while the bus is away, decisions commit and accumulate as unpublished rows, and `twalk_companion_gateway_consent_outbox_pending` says how many.
- **Schema migrations are embedded in the binary** (`src/store.rs::MIGRATIONS`, tracked by SQLite's `user_version`) and applied at open, so an operator upgrades the image and nothing else.
- **No message content, ever.** A decision is a subject, a state, a perimeter, two timestamps, the owner who took it and an optional reason.

### Taking a decision

```http
POST /api/consent/decisions
Cookie: twalk_device=<the device token sign-in issued>

{
  "subject": { "type": "contact", "id": "@whatsapp_33612345678:example.com" },
  "new_state": "granted",
  "scope": { "networks": ["whatsapp"] },
  "reason": "optional, kept in the audit trail"
}
```

`subject.type` is `contact` or `network`; `persona` is refused with `unsupported_subject_type` until persona activation lands (#60, ADR 0013 — it uses this same write path). A `network` subject must be scoped to exactly its own network. The answer is `201` with the recorded decision and the id of the event the outbox will publish, or `200` with the same body when the identical decision (same subject, state, perimeter and instant) was already recorded. Every refusal is the Gateway's `Error` document: a stable `error` code — `malformed_request`, `unknown_value`, `unsupported_subject_type`, `scope_contradicts_subject`, `consent_not_configured`, `store_unavailable`, and `unauthenticated` from the guard — with a `detail` for an operator's logs. All three endpoints and every one of those codes are declared in `openapi.yaml`.

Nothing in `src/consent_http.rs` authenticates: the guard above has already done it, and the `Device` it injected is what the handler asks for. The decision's `actor` is the deployment's **owner**, not the device — one owner per Gateway, so every device that can sign in is theirs, and the audit trail records the human; the device's id goes to the log line, where it answers "from which of my devices did I do that?".

### Precedence

A `network` decision is that network's default; a `contact` decision for the same network always overrides it. `GET /api/consent/effective?contact=<matrix id>&network=<network>` applies that:

```json
{ "contact": "@whatsapp_336…:example.com", "network": "whatsapp",
  "state": "revoked", "decided_by": { "type": "contact", "id": "@whatsapp_336…:example.com" } }
```

With no decision at all the state is `pending` and `decided_by` is `null` — which is how a caller tells "never decided" from "decided pending". An absent subject never means "revoked".

### The event

Each committed decision is published on `twalk.consent.state.changed.v1`, validated against `contracts/cloudevents/v1/consent.state.changed.schema.json`. Four conventions a third party can code against:

- `id` is the schema's recipe: `sha256(subject.type + ':' + subject.id + ':' + new_state + ':' + <networks> + ':' + occurred_at)` in lowercase hex, where `<networks>` is `scope.networks` sorted ascending and comma-joined. The scope is in the key on purpose, so two decisions differing only in perimeter are two events.
- `source` is `gateway://<the owner's server name>/consent`.
- The `network` extension is set **only** when the scope names exactly one network (so a single-network change can be filtered server-side on NATS); `data.scope.networks` is always the authority.
- The `consent` extension is **never** set: on an event announcing a consent change it would be redundant with `data.new_state` at best and self-contradictory on a revocation. Consumers read `data.new_state`.

`occurred_at` is when the user decided (part of the id); `time` is when the Gateway produced the event — they differ on a republish, which is why only the first is in the key. `old_state` is what the subject held on this perimeter before, read inside the recording transaction from the most recent decision covering any of the scoped networks; `unset` means none ever did.

Consent follows sign-in: it needs an owner to attribute a decision to, a state directory to keep the journal in and a domain to name its events by, and takes all three from the sign-in configuration. The one variable it adds is `GATEWAY_NATS_URL`; without it the consent endpoints answer `503 consent_not_configured` and the rest of the origin is untouched.

### The snapshot, and the hand-off to the bus

A consumer whose cache is cold cannot recover consent from the bus alone: a durable consumer resumes at its ack floor, so the decisions it already applied are never redelivered, and replaying the whole stream would make a confidentiality guarantee expire with a retention policy. So it asks the Gateway (ADR 0010):

```
GET /api/consent/snapshot
Authorization: Bearer <GATEWAY_SERVICE_TOKEN>

{ "stream": "twalk", "subject": "twalk.consent.state.changed.v1",
  "stream_sequence": 41, "next_stream_sequence": 42, "decision_sequence": 7,
  "entries": [ { "subject": { "type": "network", "id": "whatsapp" }, "network": "whatsapp",
                 "state": "granted", "decided_at": "…", "decision_sequence": 3 } ] }
```

Apply `entries`, then create the stream consumer at `next_stream_sequence` — `stream_sequence + 1`, spelled out because that off-by-one is the one mistake that would skip a decision. Every decision ever taken is then in **exactly one** of the two: in the snapshot, or on the stream after `stream_sequence`. Never both, never neither.

What makes that exact:

- **The snapshot is the state of the journal's published prefix.** A decision still waiting in the outbox has no position on the bus yet, so including it would hand the consumer a decision it is about to receive again. The journal remembers where each published decision landed (`stream_sequence`, written with the outbox's mark), and the snapshot stops at the last decision every decision before which also has a position. A bus outage therefore delays what the snapshot knows rather than corrupting the hand-off.
- **The position and the content are read together**, in one transaction over the one connection every write also goes through. No decision can be committed, and none marked published, between the two reads: whatever a concurrent caller does lands entirely inside the snapshot or entirely after it.
- **Nothing else is in the answer.** No timestamp, no version, no entry count: the stream sequence is the only ordering this design trusts, and ADR 0010 rejected version counters and clocks precisely so that there is nothing to arbitrate between.

Revocations are explicit, as they are in `/api/consent/state` — an absent subject means "never decided", never "revoked" — network defaults are included, and `persona` subjects are excluded (a persona's activation is a consent decision, ADR 0013, but it is not state a consumer labels senders by). The exclusion is in the SQL, so the snapshot never even reads such a row.

There is no pagination: a cursor would be a second ordering to get wrong, and a half-applied snapshot is worse than none. Instead `GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES` (100000 by default) caps it, and a state over the cap is refused with `500 snapshot_too_large` naming the variable — never truncated, because a truncated snapshot would tell a consumer that contacts the user granted were never decided about.

The caller is a service, not one of the owner's browsers: the Sensor has no Matrix OpenID token to sign in with and no cookie to send, so this route takes `GATEWAY_SERVICE_TOKEN` as an `Authorization: Bearer` credential. The two credentials are disjoint — a device token opens every other endpoint and not this one; the service token opens this one and nothing else — and the Gateway compares the token as a SHA-256 digest, so a refusal leaks neither its length nor how far a guess got. It grants a read of the whole social graph the user ever decided about, which is why it is generated (`openssl rand -hex 32`) and why the Gateway refuses to start with one under 32 characters.

## The pending contacts (ticket #54)

Screen 5 says "3 consent decisions waiting". To count them the Gateway has to know who has
written, so it consumes the bus as well as producing on it — and this is where the design is
most exposed, because a careless version of it is a log of who writes to the user.

```
GET /api/contacts/pending

{ "total": 3,
  "networks": [ { "network": "signal", "count": 1 }, { "network": "whatsapp", "count": 2 } ],
  "contacts": [ { "contact": "@whatsapp_33612345678:example.com", "network": "whatsapp",
                  "first_seen": "2026-09-17T10:00:00.000Z",
                  "last_seen":  "2026-09-17T18:30:00.000Z" } ] }
```

**Four values, and there will never be a fifth.** No message body, no display name, no
`network_identifier` — the Sensor withholds the identifier until consent is granted, and the
Gateway does not undo that by keeping a copy. That restraint is the feature, so it is enforced
rather than remembered: the projection deserialises each event into a struct with three fields
and **no `data` member**, so the body never becomes a value in the process at all; the store is a
four-column table whose columns a unit test pins; and `tests/pending.rs` publishes an event
carrying a body, a display name and an identifier, then reads the bytes of the whole state
directory and the captured logs for each of them.

The uncomfortable part, said here rather than only in a document: a bridged ghost user's Matrix
ID conventionally embeds the network identifier (`@whatsapp_33612345678:example.com`), so this
store keeps phone numbers although it has no column for one. It is stored because a consent
decision has to name its subject and that ID *is* the subject — see
`docs/architecture/security-model.md`, residual risk 5.

**Waiting** is the same question `/api/consent/effective` answers with `decided_by: null`:
neither the contact's own decision nor its network's default exists. Granting or revoking a whole
network therefore empties the list of every contact on it at once, and a contact the owner
deliberately left `pending` is not in it — they answered, and the answer was "not yet". The owner
is never in it either: their own messages travel through the same rooms, and nobody is their own
correspondent, so those sightings are not filtered out on read — they are never stored.

`total` and `networks` always count the whole list, whatever `?network=` narrows `contacts` to: a
badge and the list beside it must never disagree. There is no pagination and no cap, as for
`/api/consent/state`.

**The consumer.** Durable, created with full delivery, so a Gateway installed after weeks of
Sensor traffic builds its first list from the stream's history rather than showing an empty
inbox; every run after that resumes at its own ack floor, which is the bus's business — the
Gateway keeps no cursor of its own. Each sighting is committed and then acked, so a crash between
the two redelivers a row the store already has, and the upsert absorbs it (`first_seen` only ever
moves earlier, `last_seen` only ever later).

**Display names** are the one thing read on demand:

```
GET /api/contacts/display-names?contact=@whatsapp_33612345678:example.com

{ "contacts": [ { "contact": "@whatsapp_33612345678:example.com",
                  "display_name": "Aïcha Benali" } ] }
```

A separate call because a record of who writes to the user *and what they are called* is a
directory, and this is not one. The Gateway walks the tail of the inbound stream, keeps the most
recent name it finds for each contact asked about, and drops everything else — nothing is
written, cached or logged. A contact whose last message has fallen outside that window comes back
with `display_name: null`, which is an answer and not a failure: the Companion shows the Matrix
ID, which is what the decision will name anyway.

## Approving a suggestion (ticket #24)

A persona proposes a reply and never sends it. The act that sends it is an **approval**, and
`CONTEXT.md` defines it with enough precision that the definition is the implementation:

> The human act that turns a suggestion into an outbound reply, carrying the identity of whoever
> approved it. Deliberate by construction — an explicit call, never a default, never a batch — and
> refused if the sender's consent is no longer `granted` at that moment.

```
POST /api/approvals
{ "suggestion_event_id": "319be8ff…",
  "final": { "body": "Plutôt 20h30, si ça te va", "format": "text/plain" } }

201
{ "event_id": "57f0e4d3…", "suggestion_event_id": "319be8ff…",
  "approved_by": "@michel:example.com", "persona_id": "assistant",
  "network": "whatsapp", "contact": "@whatsapp_33612345678:example.com",
  "edited": true, "approved_at": "2026-09-17T10:04:37.000Z",
  "publication": "published", "stream_sequence": 4242,
  "published_at": "2026-09-17T10:04:37.000Z" }
```

`final` is optional — without it the persona's own words go out, and `edited` says which happened.
`approved_by` may be stated and must then be the deployment's owner: the Gateway stamps it either
way and refuses to record an approval under another name, because silently rewriting the one
identity field of an audit trail is worse than saying no.

**One suggestion.** There is no endpoint that approves a list, and a body carrying one is refused
with `approval_is_not_a_batch` rather than helpfully interpreted. A batch approval is a single
click that sends several messages, which is the thing "never a batch" forbids.

**The consent check is made now.** Not the label the suggestion was born with — that label is a
fact about when the Sensor observed the message, and it stays `granted` after the user revokes the
contact. Both are checked, and they are two different refusals: `suggestion_was_never_consented`
for the label, `consent_revoked` (or `consent_pending`) for the state as it is at the moment the
approval is given. That last one is why this endpoint is on the Gateway and not on the Hermes
runtime, which spec [#19](https://github.com/linagora/twalk/issues/19) had planned for and which
would have had to ask the Gateway over HTTP for state the Gateway itself writes —
[ADR 0022](../docs/architecture/adr/0022-the-approval-api-lives-on-the-companion-gateway.md).

**There is no queue.** A consent decision is committed and published later, because a decision the
user took must survive a bus outage. An approval is the opposite: a send held for later is a send
whose consent check has gone stale, so the reply is published inside the request or not at all. A
`201` means the bus acknowledged it and names the position; a bus that does not answer is a `502`
the user can retry. The row the Gateway then writes is bookkeeping — `GET
/api/approvals/{suggestion_event_id}` reads it back, which is how "did my reply actually go out?"
has an answer that is not a spinner — and it holds **no message content**: the suggestion's id,
who approved it, whether they edited it, and where the publication landed.

Every refusal has its own code, and the status is part of the answer rather than decoration:

| Status | Code | What happened |
| --- | --- | --- |
| `400` | `malformed_request`, `approval_is_not_a_batch`, `unknown_value` | The request is not one approval. |
| `403` | `approved_by_is_not_the_owner` | Somebody else was named as the approver. |
| `404` | `suggestion_not_found`, `trigger_not_found` | The whole retained stream was read and it is not there. |
| `409` | `suggestion_expired`, `consent_revoked`, `consent_pending`, `suggestion_was_never_consented`, `already_approved`, `trigger_has_no_room`, `suggestion_unreadable` | It exists and cannot be approved. |
| `410` | `suggestion_out_of_reach`, `trigger_out_of_reach` | The bounded search gave up before the stream's start — "I did not look that far", which is not "it is not there". |
| `502` | `bus_unreachable` | The bus did not answer. Nothing was sent. Not a `503`: this Gateway is configured and answering. |
| `503` | `approvals_not_configured` | This deployment has no bus, so it approves nothing. Answered before the suggestion is looked at. |

The suggestion and the message it answers are found by reading the bus, which has no index from an
event id to a stream position. The read is bounded by `GATEWAY_APPROVAL_LOOKUP_WINDOW`, and the
bound is **visible in the answer**: `410` and not `404`, because a user acts differently on "this
is gone" than on "this never existed". The trigger's search is anchored on the suggestion's own
position rather than on the stream's head, so a persona activated today — which reads the stream
from the beginning (ADR 0013) — can still have its first suggestions approved, although the
messages they answer are weeks old.

One gap, named rather than papered over: the approved reply carries no `target.reply_to_event_id`,
so the Sensor posts it into the portal room without threading it under the original. The contract's
inbound event carries no Matrix event ID of its own to thread under, and inventing one would be
worse than the gap.

## The model and the language (ticket #98)

Twalk ships no LLM and has **no default**: nothing is ever sent to a model the operator did not
name, and a persona refuses to start without one ([ADR 0015](../docs/architecture/adr/0015-no-default-llm-configured-through-the-companion.md)).
The Gateway is where that choice lives, because the operator must be able to change it **without
shell access**, and the Hermes runtime reads it here and injects it into each persona's
environment rather than letting a persona fetch it — the token that opens this configuration is
the token that opens the consent snapshot, the list of every contact, and a third-party persona
takes exactly the same path as a first-party one (ADR 0008).

The user's native language lives here for the same reason
([ADR 0016](../docs/architecture/adr/0016-a-reply-follows-the-conversation-not-the-user.md)): a
persona runs in a container and cannot read `navigator.language`, and it needs a language to fall
back to when it cannot tell what language the message it is answering was written in. Until this
ticket that fallback had no value to fall back to, and a French speaker writing `test` got an
English draft ([#164](https://github.com/linagora/twalk/issues/164)).

### The shape the reference deployment has, and the one to copy

An **OpenAI-compatible proxy in front of the model**. The reference deployment runs LiteLLM in
front of Qwen at OVH, so Twalk sees this and nothing more:

```
base_url : http://127.0.0.1:4000/v1
model    : qwen
key      : a file on the host
```

That is the recommended shape, and it is what the documentation leads with on purpose. **Every
provider peculiarity belongs in the proxy**, not in Twalk: `drop_params`,
`additional_drop_params`, `reasoning_effort`, the provider's real model id. An operator reading
this project a month ago would reasonably have concluded they must enumerate their provider's
quirks in Twalk's own configuration, and they should not.

`params` — the free-form provider passthrough — stays, because an operator with no proxy needs it:
providers differ in what they reject, and OVH's AI Endpoints, the first endpoint tried in
practice, rejects fields OpenAI clients send by default. It is merged into every request untouched
and **last**, and a member set to `null` removes a field the request would otherwise carry. It is
the **escape hatch**, not the norm, and an empty one is the healthy shape.

### The credential: a file wins, and it is write-only

`GATEWAY_LLM_API_KEY_FILE` names a file on the host. If it is set, **that credential is the one in
force**, whatever the Companion last wrote (ADR 0015) — so a production stack locks the credential
down while the model name still comes from the browser, and a developer with no file stays in the
browser entirely. That is not a theoretical precedence: it is how the reference deployment runs,
so it is the path built first.

The credential is **write-only over the API**. `PUT` takes it; no read returns it. A browser is
told three things — that one is configured, which of the two sources is in force, and its last
four characters — which is enough for a human to recognise the key they pasted and not enough for
anyone to use it. The one answer that carries the credential is `GET /api/settings/runtime`,
behind the service token, whose caller is the process that injects it.

Because the credential is write-only, a client cannot round-trip it, so `PUT` has three cases:
the `credential` member **absent** keeps what is stored (a screen that only renamed the model has
nothing to send back), `null` forgets the one set from the browser, and a string replaces it. A
member the shape does not have is refused rather than ignored — a client that sent `credentials`
for `credential` would otherwise believe it had set a key it had not.

An empty or unreadable credential file is a **startup failure** naming the variable, not a silent
fall back to the browser's value: an operator who named a file meant to supply a credential, and
falling back would be the precedence rule failing in the direction it exists to prevent.

The credential lands in `settings.sqlite3` in `GATEWAY_STATE_DIR`, which **nothing encrypts at
rest** ([#14](https://github.com/linagora/twalk/issues/14)). It is an outbound secret that bills
money and sees message content; it is in `docs/architecture/security-model.md`'s credential table
and residual risks, and `tests/settings.rs` reads the file's own bytes so the document and the code
cannot drift apart quietly.

### Four causes, four answers

`POST /api/settings/model/probe` sends **one chat completion of one token** to the configured
endpoint, with the operator's provider parameters merged in exactly as a persona merges them — so
what it proves is what a persona will do. It is the only endpoint on this origin that spends the
operator's money, and only when a human asks; the prompt is the word `ping`, and no message
content reaches the endpoint.

| answer | what happened |
| --- | --- |
| `200` `outcome: ok` | the endpoint answered a chat completion |
| `409 model_not_configured` | nothing is configured to probe |
| `502 endpoint_unreachable` | nothing answered: DNS, connection, TLS, or the ten-second deadline. `endpoint_status` is `null`, because there was no answer to have one |
| `502 endpoint_refused` | it answered and said no; `endpoint_status` is its own status, and `detail` its own message |
| `502 endpoint_not_compatible` | it answered a success that is not a chat completion — the wrong port, almost always |

That separation is the point. A persona that cannot reach the endpoint, one whose request the
endpoint refused, and one with no endpoint configured at all must not share a signal: this project
has produced nine incidents in two days from failures that did
([#116](https://github.com/linagora/twalk/issues/116),
[#141](https://github.com/linagora/twalk/issues/141)). The same trichotomy holds one level up, for
the runtime's own read: a Gateway it cannot reach is a transport failure, a wrong token is `401`,
and a Gateway with no model configured answers `200` with `llm: null` — three facts, three
signals, none of them an empty document that could be mistaken for another.

A success does *not* mean the model produced text. With a budget of one token a reasoning model
spends it thinking and answers with no content
([#162](https://github.com/linagora/twalk/issues/162)), and that is still a reachable, willing
endpoint that knows this model's name.

### The endpoints

| | |
| --- | --- |
| `GET /api/settings/model` | the endpoint, the model, the passthrough, and the credential *described* |
| `PUT /api/settings/model` | name them |
| `DELETE /api/settings/model` | forget them, and the credential set from the browser |
| `POST /api/settings/model/probe` | the four answers above |
| `GET /api/settings/language` | the preference, and the five the Companion ships |
| `PUT /api/settings/language` | set it, or `null` for no preference — which is *not* English |
| `GET /api/settings/runtime` | everything the Hermes runtime injects, in one read. **Service token** |

Every document carries `personas`, and in v0.1 it is always `{}`. Shipping a working per-persona
override with one persona would ship an unexercised path; adding the member later would change
the shape of an endpoint clients had already generated against. The member exists and is empty,
which is the only one of the three options that costs nothing later.

`#101` is the Companion's settings screen; this is the API it draws from.

## Configuration

Environment variables only, like the Sensor. They are documented for an operator in `deploy/docker-compose/.env.example`.

| Variable | Default | Meaning |
| --- | --- | --- |
| `GATEWAY_LISTEN` | `0.0.0.0:8080` | Address the origin listens on. Port `0` asks the kernel for a free port (what the test suite uses). |
| `GATEWAY_STATIC_DIR` | *required* | Directory the Companion's build is served from. Absent or empty is not fatal: the origin answers a clear `404` while health and metrics stay up. |
| `GATEWAY_FALLBACK_FILE` | `200.html` | The build's SPA fallback file, served with `200` for any client-side route. |
| `GATEWAY_LOG_LEVEL` | `info` | `tracing` filter, e.g. `info,twalk_companion_gateway=debug`. |
| `GATEWAY_OWNER` | *unset* | The Matrix ID of the one human this deployment serves. Unset: nobody can sign in and the API answers `503`. |
| `GATEWAY_HOMESERVER_FEDERATION_URL` | *required with an owner* | Base URL of the homeserver's federation API, where an OpenID token is verified. |
| `GATEWAY_NATS_URL` | *unset* | The bus the consent outbox publishes to, e.g. `nats://nats:4222`. Unset: the consent endpoints answer `503`. A bus that is down delays publication and never refuses a decision. |
| `GATEWAY_STATE_DIR` | *required with an owner* | Directory the SQLite stores live in: `sessions.db`, `consent.sqlite3` and `settings.sqlite3`. |
| `GATEWAY_DEVICE_TOKEN_TTL` | `900` | Device-token lifetime in seconds. |
| `GATEWAY_REFRESH_TOKEN_TTL` | `2592000` | Refresh-token lifetime in seconds. |
| `GATEWAY_HOMESERVER_URL` | the federation URL | Base URL of the homeserver's **client** API, where registration is relayed and invitations are sent. Same host and port as the federation URL in the reference deployment, hence the default. |
| `GATEWAY_REGISTRATION_SHARED_SECRET` | *unset* | The homeserver's registration shared secret. Unset: the registration relay is off and `POST /api/bootstrap/account` answers `503`. |
| `GATEWAY_SENSOR_USER_ID` | *unset* | The Sensor's Matrix ID — who gets invited. Unset: `POST /api/bootstrap/rooms` answers `503`. |
| `GATEWAY_SERVICE_TOKEN` | *unset* | The token the consent snapshot **and the runtime settings** are read with — the Sensor's and the Hermes runtime's credential, and the only two endpoints it opens. Unset: `GET /api/consent/snapshot` and `GET /api/settings/runtime` answer `503`. Shorter than 32 characters: the Gateway refuses to start. |
| `GATEWAY_LLM_API_KEY_FILE` | *unset* | A path **on this host** holding the LLM endpoint's credential and nothing else, mounted read-only into the container. It **wins** over whatever the Companion set (ADR 0015), so a production stack can lock the credential down while the model name still comes from the browser. Read once at startup, as the runtime reads its own, so a rotated file takes effect at the next restart. Empty or unreadable: the Gateway refuses to start, rather than quietly using the browser's value. |
| `GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES` | `100000` | The largest snapshot served. Over it, an error — never a truncation. |
| `GATEWAY_APPROVAL_LOOKUP_WINDOW` | `20000` | How many stream positions back an approval searches for the suggestion it names, and for the message that suggestion answers. The bound is visible in the answer: a suggestion the search did not reach is `410 suggestion_out_of_reach`, never `404 suggestion_not_found`. Widen it on a bus carrying far more than one person's conversations; the cost is a longer read on the approval path alone. |
| `GATEWAY_INBOUND_CONSUMER` | `companion-gateway-pending-contacts` | The durable JetStream consumer the pending-contact projection reads through. One Gateway per deployment owns it; rename it only for a second Gateway on the same bus, which would otherwise split the stream with the first. |
| `GATEWAY_BRIDGES` | *unset* | The bridge instances this deployment can log in to, by `bridge_id`, comma-separated and in the order the Companion offers them (`mautrix-whatsapp,mautrix-signal`). Unset: `GET /api/bridges` answers an empty list. |
| `GATEWAY_BRIDGE_<ID>_URL` | *required per bridge* | That bridge's appservice listener, where its provisioning API is — e.g. `http://bridge-whatsapp:29318`. `<ID>` is the `bridge_id` upper-cased with every non-alphanumeric character as `_`. |
| `GATEWAY_BRIDGE_<ID>_PROVISIONING_SECRET` | *required per bridge* | The same value as that bridge's `provisioning.shared_secret`. It drives logins and logouts on the user's account. |
| `GATEWAY_BRIDGE_<ID>_NETWORK` | the id without `mautrix-` | The network the user experiences (`whatsapp`, `signal`, `sms`) — what the Companion labels the screen with. `mautrix-gmessages` sets it, because its network is `sms`. It must be one of the contract's networks: it is what `bridge.status.changed` carries, so the Gateway refuses to start with anything else. |
| `GATEWAY_BRIDGE_<ID>_AS_TOKEN` | *unset* | The same value as that bridge's `appservice.as_token`, which is what it authenticates its status pushes with. The Gateway uses it to **verify** a push and for nothing else. Unset: that bridge's webhook answers `503 as_token_not_configured` — an unverified push is refused, never trusted. |
| `GATEWAY_BRIDGE_<ID>_STATUS_ID` | the id without `mautrix-`, under `bridge-` | The `bridge_id` this instance's events carry (`^bridge-[a-z0-9-]+$`) and the segment of its status webhook's URL. It is an identity third parties read off the bus, so it is stable across restarts. |

SIGTERM (or SIGINT) drains in-flight requests, then exits `0`.

## Build and test

Its own Cargo package with its own lockfile and target directory — there is no workspace (ADR 0008):

```bash
cd companion-gateway
cargo test                    # unit tests, the process-boundary suites, and the compose deployment test
cargo test --test service     # the origin's own suite alone (no Docker)
cargo test --test signin      # sign-in against the shared test stack's Synapse
cargo test --test consent     # consent against the shared test stack's Synapse and NATS JetStream
cargo test --test bootstrap   # the registration relay and the Sensor's invitation, same stack
cargo test --test consent_snapshot  # the snapshot, and the hand-off to the bus
cargo test --test bridges     # the bridge login facade against a stub bridge
cargo test --test bridge_status  # the status webhook, the mapping table and the reconciliation
cargo test --test pending     # the pending-contact projection against a real bus
cargo test --test approvals   # approving a suggestion, and every way it is refused
cargo test --test settings    # the model configuration, the language, and the probe's four answers
cargo test --test openapi     # the description against the running binary
```

`tests/service.rs` runs the real binary and talks to it over HTTP; `tests/openapi.rs` does the same against `openapi.yaml` (two of its four checks need no process at all); `tests/signin.rs` and `tests/bootstrap.rs` do the same with the shared test stack up, so a real Synapse mints the OpenID tokens (its listener serves the `openid` resource for exactly that) and answers the admin registration call; `tests/consent.rs` signs a device in the same way and then drives decisions over HTTP against a real NATS JetStream, including the crash property — it kills the process with a decision still unpublished and asserts that the restart publishes it exactly once; `tests/consent_snapshot.rs` drives the hand-off itself: it takes decisions, reads the snapshot, takes more, and then creates a durable consumer at the sequence the snapshot named plus one, asserting that every later decision arrives exactly once and no earlier one arrives at all; `tests/bridges.rs` runs a stub bridge inside the test process and drives a whole login through the Gateway against it; `tests/pending.rs` publishes contract-valid inbound events onto a real JetStream and asserts the five properties of the projection — that a Gateway started against a stream with history builds its list from it, that a decision taken through the write API moves the contact out of the list, that a restart resumes at its ack floor instead of replaying, that display names come from the bus and are written nowhere, and that the store's own bytes hold no body and no network identifier; `tests/settings.rs` drives the model configuration end to end against the real binary — the reference deployment's combination (the model from the browser, the key from a file) first, the write-only rule asserted by searching every answer *and* every log line for both credentials, and the probe's four answers against a stub that works, one that refuses, one that answers something that is not a completion, and an address nothing listens on; `tests/deployment.rs` brings the `companion-gateway` service of `deploy/docker-compose/compose.yaml` up and asserts the same properties of the deployed image — and, for bootstrap, brings the `sensor` service up beside it, so that the invited room really is joined and its traffic really reaches the bus — which is also the one place a **real Sensor** publishes into a bus a real Gateway consumes, so that is where "messages published by a Sensor become contacts waiting for a decision" is asserted. They all reuse the shared harness crate (`tests/harness/`); what is Gateway-specific lives in `tests/harness/mod.rs`.

The image (`deploy/docker-compose/companion-gateway.Dockerfile`) is multi-stage: a Node stage produces the Companion's static files — a holding page until the Companion's own lot lands — the Rust stage builds the binary, and the runtime carries the two. Pass `--build-arg TWALK_BUILD_REVISION=$(git describe --always --dirty)` to have the health endpoint report the revision; the build context carries no `.git`, so without it the revision is `unknown`.
