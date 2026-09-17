# AGENTS.md

## Project overview

`twalk` is the monorepo for Twalk, a sovereign, open-source, self-hosted event hub for personal multi-channel messaging (WhatsApp, Signal, SMS, Telegram, Discord). Messaging traffic lands in Matrix portal rooms via Mautrix bridges, is normalised by the Sensor into versioned CloudEvents on a NATS JetStream bus, and is consumed by Hermes-hosted personas under human oversight. See `README.md` for the full picture and `CONTEXT.md` for the domain vocabulary.

As of 2026-09-17 the repository contains documentation, the complete event contract, and the shared integration-test harness — no component implementation yet:

- `README.md`, `docs/wireframes/companion-v0.1.md`, `docs/architecture/roadmap.md` (milestones ↔ lots), `docs/architecture/adr/` (ADRs 0005–0009 written; numbers 0001–0004 are referenced from the docs but not yet written)
- `contracts/cloudevents/v1/` — the complete v1 contract: 8 CloudEvents schemas plus one validated fixture per type, and `fixtures/variants/<type>/<variant>.json` for a shape the schema only allows under a condition (a revoked sender's reduced message, ADR 0012)
- `.scratch/sensor/` — local mirrors of the Sensor spec and tickets 01–11; **canonical is the tracker: GitHub Issues on [linagora/twalk](https://github.com/linagora/twalk/issues)** (conventions in `docs/agents/issue-tracker.md`); all 11 tickets are done and merged
- `sensor/` — the Sensor Cargo package: library modules (`config`, `consent`, `metrics`, `network`, `normalize`, `outbound` — pure logic) plus the binary wiring to matrix-sdk and NATS in `src/main.rs`; integration tests in `sensor/tests/` (`smoke.rs`, `sensor_lifecycle.rs`, `message_to_event.rs`, `consent.rs`, `reactions.rs`, `presence.rs`, `outbound.rs`, `persistence.rs`, `fidelity.rs`, `encryption.rs`, `observability.rs`, `deployment.rs`, `bridges_deployment.rs`) over the Sensor-specific half of the harness in `sensor/tests/harness/` (the Matrix `Bot`, the portal helpers, `SensorProc`, `crypto.rs` for Megolm-capable bots)
- `tests/harness/` — the `twalk-test-harness` Cargo package, shared by every component's suite: the test stack's lifecycle (`compose.test.yaml`, Synapse config, bot provisioning), the `Bus`, contract validation, `poll_until`, and a stub OpenAI-compatible LLM for persona tests. A dev dependency of `sensor/` and `hermes/`
- `hermes/` — the `twalk-hermes` Cargo package: the runtime is not implemented yet; `tests/smoke.rs` exercises the shared harness against the real stack
- `companion-gateway/` — the `twalk-companion-gateway` Cargo package: the service skeleton (ticket #48) — `config`, `http`, `static_files`, `metrics`, `trace` plus the binary in `src/main.rs`; the origin that serves the Companion's static files, `/health`, `/metrics` — the user's session (ticket #52): `matrix_openid` (verifying a Matrix OpenID token at the homeserver, with the domain check), `session` (the owner check, the replay ledger, the device list and the per-device tokens in a SQLite store) and `session_http` (the session routes and the guard that requires a device token on every `/api` route by default) — bootstrap (ticket #53): `bootstrap` and `bootstrap_http` (the registration relay that creates this deployment's one account with Synapse's registration shared secret and refuses any other, and the Sensor's invitation into the rooms the user selects, with the user's Matrix access token as a parameter of that one call) — and its own HTTP description (ticket #63): `companion-gateway/openapi.yaml`, an OpenAPI 3.1 description of the whole origin, served at `/openapi.yaml` by `openapi` (which embeds it) and **extended by every ticket that adds an endpoint**, because `tests/openapi.rs` fails on a route it does not describe. Integration tests in `companion-gateway/tests/` (`service.rs`, `signin.rs`, `bootstrap.rs` and `openapi.rs` at the process boundary, `deployment.rs` against the compose stack — where the Sensor runs beside the Gateway, joins the room it is invited to and publishes that room's traffic as `network=matrix`)
- `deploy/docker-compose/` — the reference deployment: Synapse + NATS JetStream + the Sensor + the Companion Gateway wired together, plus the `mautrix-whatsapp` and `mautrix-signal` bridges behind the `bridges` compose profile (`compose.yaml`, documented `.env.example`, `provision.sh` account provisioning, `provision-bridges.sh` appservice registrations, Synapse config template, one Dockerfile per component), covered end-to-end by `sensor/tests/deployment.rs`, `sensor/tests/bridges_deployment.rs` and `companion-gateway/tests/deployment.rs`
- `bridges/` — the bridges' base configurations (`mautrix-whatsapp/config.yaml`, `mautrix-signal/config.yaml`) and `generate-registration.sh`, the generator the deployment's registration one-shots run; no fork of any bridge (ticket #73)
- Other component directories hold stub READMEs only: `companion/`, `ui/`, `sdk/`, `examples/`, `tools/`

## Build and test commands

Requires Docker (the harness brings up its own Synapse + NATS stack) and a Rust toolchain.

Four Cargo packages, each with its own lockfile and target directory — there is no workspace, so build and test each from its own directory:

```bash
cd sensor
cargo build --tests   # compile / typecheck
cargo test            # full suite: boots the stack itself, ~15s cold, ~3s warm

cd ../hermes
cargo test            # Hermes suite: shares the same test stack

cd ../companion-gateway
cargo test            # Gateway suite: the binary at its process boundary, plus its compose service

cd ../tests/harness
cargo test            # the shared harness's own unit tests (no Docker)
```

The Sensor and Hermes suites bring up the same test stack (one Synapse, one NATS JetStream), each on its own bus subjects; `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT` and `TWALK_TEST_NATS_PORT` move a stack aside for a parallel worktree. The Companion Gateway's suite uses the same stack for its sign-in and bootstrap tests (real Synapse mints the OpenID tokens and answers the admin registration call; the test Synapse serves the `openid` resource for it); its process-boundary tests ask the kernel for a free port, and its deployment test uses the deploy-stack variables (`TWALK_DEPLOY_TEST_STACK`, `TWALK_DEPLOY_TEST_GATEWAY_PORT`, `TWALK_DEPLOY_TEST_SYNAPSE_PORT`, `TWALK_DEPLOY_TEST_NATS_PORT`) the Sensor's deployment test established — its bootstrap test is the one that needs the bus, since it asserts the invited room's traffic on it. `sensor/tests/bridges_deployment.rs` runs the reference deployment a third time, with the `bridges` profile on, under its own `TWALK_BRIDGES_TEST_*` project, ports and teardown flag — its module docs list them.

## Code organization

Monorepo with 13 top-level directories — see the "Repository layout" section of `README.md`. Stacks: Rust for `sensor/`, `hermes/` and `companion-gateway/`, SvelteKit (static export) for `companion/`. The Sensor test harness speaks the Matrix client-server API over plain HTTP — the HTTP `Bot` helpers deliberately do not depend on matrix-sdk; the exception is the `CryptoBot` helper (`sensor/tests/harness/crypto.rs`), which runs matrix-sdk with its crypto stack because raw HTTP cannot Megolm-encrypt.

## Code style guidelines

In all naming and prose, follow the `CONTEXT.md` vocabulary: **network** (never "channel" outside user-facing copy, never a bridge name like `gmessages`), **persona** (never "bot" for agent identities — test Matrix users standing in for bridges are "test bots"), **the Companion** means the PWA only, **Companion Gateway** always in full.

## Testing instructions

Tests live at the agreed seam: the component's process boundary (real Synapse + real NATS JetStream). What every suite shares is in the `tests/harness/` crate (stack lifecycle, `Bus`, `validate_against_contract`, `poll_until`, the stub LLM); component-specific helpers stay with their component (`sensor/tests/harness/` re-exports the crate so Sensor test files see one flat `harness::` namespace). New test files go beside `smoke.rs`. Every event-producing behaviour must assert schema validity via `validate_against_contract`. Keep tests isolated from persisted stack state (unique rooms and bus subjects per run).

## Security considerations

Never commit secrets, credentials, or `.env` files (`.gitignore` covers `.env`). The only secrets in the repo are the throwaway constants in `tests/harness/synapse/homeserver.yaml` and the test-bot passwords — local, ephemeral test stack only; never reuse them elsewhere. Message content is highly sensitive by design: no message may leave the user's infrastructure without explicit consent — keep that invariant in mind for any code that touches event payloads.

## Agent skills

### Issue tracker

GitHub Issues on [linagora/twalk](https://github.com/linagora/twalk/issues), with parent `spec` issues, native dependencies and one PR per ticket. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical roles, each label named after its role, plus this repo's own `spec`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` at the root, ADRs in `docs/architecture/adr/`. See `docs/agents/domain.md`.
