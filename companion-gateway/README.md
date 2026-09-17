# companion-gateway

The Companion backend (Rust): bridge provisioning facade, persona orchestrator, and consent broker. It is the single writer of consent state (see `docs/architecture/adr/0006-consent-state-owned-by-companion-gateway.md`).

What exists today is the service skeleton (ticket #48): the origin that serves the Companion, a health endpoint, metrics, structured logs and a graceful shutdown. Consent, authentication and the bridge facade land on top of it in the later tickets of spec #46.

## The origin

One HTTP origin serves everything, so there is no CORS and the device token can later be an `HttpOnly` cookie (ADR 0011):

| Path | Answer |
| --- | --- |
| `/health` | `200` with `{"status":"ok","version":"…","revision":"…"}`. `version` is the server half of the Companion's version handshake: the PWA compares it with the version baked into its own build and reloads when a service worker has left it holding a stale app shell. |
| `/metrics` | The Prometheus text exposition, in the Sensor's conventions (`twalk_companion_gateway_*`). |
| `/api/…` | The Gateway's own API surface — empty in this skeleton, so every path under it is a JSON `404`. An API path never answers with HTML: a client parsing a response must not be handed a page. |
| anything else | The Companion's build in `GATEWAY_STATIC_DIR`, resolved the way SvelteKit's static adapter lays it out (see below). |

### Resolving a path to a file of the Companion's build

The Companion is a SvelteKit static export: a prerendered page is a file (`onboarding/whatsapp.html`, or `onboarding/signal/index.html` when the build keeps trailing slashes), and a route that was not prerendered exists only in the client-side router. So `src/static_files.rs` follows the adapter's own preview-server order: the exact file; else the path plus `index.html` (trailing slash) or plus `.html`; else a `307` to whichever trailing-slash spelling the build does have; else the SPA fallback, with `200`.

- The fallback is `200.html` (`GATEWAY_FALLBACK_FILE`), not `index.html` — the adapter's documentation warns that an `index.html` fallback collides with a prerendered homepage.
- Content types come from the path's extension and default to `text/html`. `.wasm` is exactly `application/wasm`, with no parameters: `WebAssembly.instantiateStreaming` rejects anything else with a `TypeError` and the Companion has no fallback path, so a stray `charset` would break onboarding with an error that looks nothing like a MIME problem.
- A pre-compressed sibling (`crypto.wasm.br`, `crypto.wasm.gz`) is served, with its `Content-Encoding`, to a client that accepts that encoding — the Matrix crypto module is ~7.5 MB raw against ~1.3 MB brotli-compressed.

A `traceparent` on an inbound request is continued (and returned on the response); a request without one, or with a malformed one, gets a fresh W3C trace context. Per-request log lines (method, path, status, duration, `traceparent`) are at `debug`; the lifecycle is at `info`.

## Configuration

Environment variables only, like the Sensor. They are documented for an operator in `deploy/docker-compose/.env.example`.

| Variable | Default | Meaning |
| --- | --- | --- |
| `GATEWAY_LISTEN` | `0.0.0.0:8080` | Address the origin listens on. Port `0` asks the kernel for a free port (what the test suite uses). |
| `GATEWAY_STATIC_DIR` | *required* | Directory the Companion's build is served from. Absent or empty is not fatal: the origin answers a clear `404` while health and metrics stay up. |
| `GATEWAY_FALLBACK_FILE` | `200.html` | The build's SPA fallback file, served with `200` for any client-side route. |
| `GATEWAY_LOG_LEVEL` | `info` | `tracing` filter, e.g. `info,twalk_companion_gateway=debug`. |

SIGTERM (or SIGINT) drains in-flight requests, then exits `0`.

## Build and test

Its own Cargo package with its own lockfile and target directory — there is no workspace (ADR 0008):

```bash
cd companion-gateway
cargo test                 # unit tests, the process-boundary suite, and the compose deployment test
cargo test --test service  # the process-boundary suite alone (no Docker)
```

`tests/service.rs` runs the real binary and talks to it over HTTP; `tests/deployment.rs` brings the `companion-gateway` service of `deploy/docker-compose/compose.yaml` up and asserts the same properties of the deployed image. Both reuse the shared harness crate (`tests/harness/`); what is Gateway-specific lives in `tests/harness/mod.rs`.

The image (`deploy/docker-compose/companion-gateway.Dockerfile`) is multi-stage: a Node stage produces the Companion's static files — a holding page until the Companion's own lot lands — the Rust stage builds the binary, and the runtime carries the two. Pass `--build-arg TWALK_BUILD_REVISION=$(git describe --always --dirty)` to have the health endpoint report the revision; the build context carries no `.git`, so without it the revision is `unknown`.
