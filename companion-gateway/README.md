# companion-gateway

The Companion backend (Rust): bridge provisioning facade, persona orchestrator, and consent broker. It is the single writer of consent state (see `docs/architecture/adr/0006-consent-state-owned-by-companion-gateway.md`).

What exists today is the service skeleton (ticket #48) — the origin that serves the Companion, a health endpoint, metrics, structured logs and a graceful shutdown — plus the user's session (ticket #52): sign-in with a Matrix OpenID token, the owner check, and per-device tokens with revocation. Consent and the bridge facade land on top of them in the remaining tickets of spec #46.

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

A `traceparent` on an inbound request is continued (and returned on the response); a request without one, or with a malformed one, gets a fresh W3C trace context. Per-request log lines (method, path, status, duration, `traceparent`) are at `debug`; the lifecycle is at `info`.

### The HTTP description (ticket #63)

`openapi.yaml`, next to this README, is an OpenAPI 3.1 description of the whole origin: every endpoint, its authentication, its response shapes, and — per status — the machine-readable `error` code a client branches on. The Companion is built in its own lot, by another agent, from a TypeScript client generated against it. That is the property the CloudEvents contract gives the bus, applied to this origin: components built in parallel without coordination, and a field that changes breaks a build instead of an onboarding screen.

- **Where it lives.** With the component, not in `contracts/`. `contracts/` is the *bus* contract — CloudEvents schemas and fixtures, released CC0, shared by every component and consumed by third parties; this describes one component's own HTTP surface, moves with that component's code, and is embedded in its binary with `include_str!`. Putting it in `contracts/` would have widened what that directory means and coupled a crate's build to a path outside it.
- **Which way the check runs.** The description is the source of truth and the implementation is checked against it. `tests/openapi.rs` reads the file, drives the real binary through every operation it declares, and asserts the status, the media type, the body against the response's own JSON Schema (OpenAPI 3.1 schemas *are* JSON Schema 2020-12 — that is why 3.1), the `error` code, and the authentication each operation declares against `session_http::requirement` itself. It also asserts that every route the router registers is described and every described path is a route. Generating the description from the handlers would have described whatever they happen to do, mistakes included, and the Companion's lot would generate a client from a document nobody reviewed.
- **The obligation on later tickets.** Every ticket of spec #46 that adds an endpoint extends `openapi.yaml` in the same commit. It is not a convention to remember: an undescribed route fails the suite.

`GET /openapi.yaml` serves the committed file's bytes (`application/yaml`, RFC 9512), unauthenticated — a generator must be able to read it before anyone can sign in, and it holds no secret.

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
| `GATEWAY_STATE_DIR` | *required with an owner* | Directory the SQLite stores live in; the session store is `sessions.db` inside it. |
| `GATEWAY_DEVICE_TOKEN_TTL` | `900` | Device-token lifetime in seconds. |
| `GATEWAY_REFRESH_TOKEN_TTL` | `2592000` | Refresh-token lifetime in seconds. |

SIGTERM (or SIGINT) drains in-flight requests, then exits `0`.

## Build and test

Its own Cargo package with its own lockfile and target directory — there is no workspace (ADR 0008):

```bash
cd companion-gateway
cargo test                 # unit tests, the process-boundary suites, and the compose deployment test
cargo test --test service  # the origin's own suite alone (no Docker)
cargo test --test signin   # sign-in against the shared test stack's Synapse
cargo test --test openapi  # the description against the running binary
```

`tests/service.rs` runs the real binary and talks to it over HTTP; `tests/openapi.rs` does the same against `openapi.yaml` (two of its four checks need no process at all); `tests/signin.rs` does the same with the shared test stack up, so a real Synapse mints the OpenID tokens (its listener serves the `openid` resource for exactly that); `tests/deployment.rs` brings the `companion-gateway` service of `deploy/docker-compose/compose.yaml` up and asserts the same properties of the deployed image. Both reuse the shared harness crate (`tests/harness/`); what is Gateway-specific lives in `tests/harness/mod.rs`.

The image (`deploy/docker-compose/companion-gateway.Dockerfile`) is multi-stage: a Node stage produces the Companion's static files — a holding page until the Companion's own lot lands — the Rust stage builds the binary, and the runtime carries the two. Pass `--build-arg TWALK_BUILD_REVISION=$(git describe --always --dirty)` to have the health endpoint report the revision; the build context carries no `.git`, so without it the revision is `unknown`.
