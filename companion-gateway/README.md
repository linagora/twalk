# companion-gateway

The Companion backend (Rust): bridge provisioning facade, persona orchestrator, and consent broker. It is the single writer of consent state (see `docs/architecture/adr/0006-consent-state-owned-by-companion-gateway.md`).

What exists today is the service skeleton (ticket #48) — the origin that serves the Companion, a health endpoint, metrics, structured logs and a graceful shutdown — the user's session (ticket #52): sign-in with a Matrix OpenID token, the owner check, and per-device tokens with revocation — the consent store (ticket #49): an append-only decision journal in SQLite, the current state as its projection, and a transactional outbox that publishes each committed decision exactly once as a `consent.state.changed.v1` — and bootstrap (ticket #53): the registration relay that creates this deployment's one account, and the Sensor's invitation into the rooms the user chooses. The consent snapshot (#50) and the bridge facade land on top of them in the remaining tickets of spec #46.

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

Authentication is the default and not an opt-in: everything under `/api/` requires a live device token unless `session_http::requirement` says otherwise, so a route added by a later ticket is protected before its author writes a line of authentication code. The exceptions are the sign-in itself, the refresh (which authenticates the refresh cookie and rotates it), and — when ticket #50 lands — the consent snapshot, which takes a service token because the Sensor is not a device: it adds one line to that table, and the guard then asks for no device cookie there and injects no device identity, leaving the snapshot handler to check the service token from its own configuration.

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
| `GATEWAY_STATE_DIR` | *required with an owner* | Directory the SQLite stores live in; the session store is `sessions.db` inside it. |
| `GATEWAY_DEVICE_TOKEN_TTL` | `900` | Device-token lifetime in seconds. |
| `GATEWAY_REFRESH_TOKEN_TTL` | `2592000` | Refresh-token lifetime in seconds. |
| `GATEWAY_HOMESERVER_URL` | the federation URL | Base URL of the homeserver's **client** API, where registration is relayed and invitations are sent. Same host and port as the federation URL in the reference deployment, hence the default. |
| `GATEWAY_REGISTRATION_SHARED_SECRET` | *unset* | The homeserver's registration shared secret. Unset: the registration relay is off and `POST /api/bootstrap/account` answers `503`. |
| `GATEWAY_SENSOR_USER_ID` | *unset* | The Sensor's Matrix ID — who gets invited. Unset: `POST /api/bootstrap/rooms` answers `503`. |
| `GATEWAY_BRIDGES` | *unset* | The bridge instances this deployment can log in to, by `bridge_id`, comma-separated and in the order the Companion offers them (`mautrix-whatsapp,mautrix-signal`). Unset: `GET /api/bridges` answers an empty list. |
| `GATEWAY_BRIDGE_<ID>_URL` | *required per bridge* | That bridge's appservice listener, where its provisioning API is — e.g. `http://bridge-whatsapp:29318`. `<ID>` is the `bridge_id` upper-cased with every non-alphanumeric character as `_`. |
| `GATEWAY_BRIDGE_<ID>_PROVISIONING_SECRET` | *required per bridge* | The same value as that bridge's `provisioning.shared_secret`. It drives logins and logouts on the user's account. |
| `GATEWAY_BRIDGE_<ID>_NETWORK` | the id without `mautrix-` | The network the user experiences (`whatsapp`, `signal`, `sms`) — what the Companion labels the screen with. `mautrix-gmessages` sets it, because its network is `sms`. |

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
cargo test --test bridges     # the bridge login facade against a stub bridge
cargo test --test openapi     # the description against the running binary
```

`tests/service.rs` runs the real binary and talks to it over HTTP; `tests/openapi.rs` does the same against `openapi.yaml` (two of its four checks need no process at all); `tests/signin.rs` and `tests/bootstrap.rs` do the same with the shared test stack up, so a real Synapse mints the OpenID tokens (its listener serves the `openid` resource for exactly that) and answers the admin registration call; `tests/consent.rs` signs a device in the same way and then drives decisions over HTTP against a real NATS JetStream, including the crash property — it kills the process with a decision still unpublished and asserts that the restart publishes it exactly once; `tests/bridges.rs` runs a stub bridge inside the test process and drives a whole login through the Gateway against it; `tests/deployment.rs` brings the `companion-gateway` service of `deploy/docker-compose/compose.yaml` up and asserts the same properties of the deployed image — and, for bootstrap, brings the `sensor` service up beside it, so that the invited room really is joined and its traffic really reaches the bus. They all reuse the shared harness crate (`tests/harness/`); what is Gateway-specific lives in `tests/harness/mod.rs`.

The image (`deploy/docker-compose/companion-gateway.Dockerfile`) is multi-stage: a Node stage produces the Companion's static files — a holding page until the Companion's own lot lands — the Rust stage builds the binary, and the runtime carries the two. Pass `--build-arg TWALK_BUILD_REVISION=$(git describe --always --dirty)` to have the health endpoint report the revision; the build context carries no `.git`, so without it the revision is `unknown`.
