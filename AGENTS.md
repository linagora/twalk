# AGENTS.md

## Project overview

`twalk` is the monorepo for Twalk, a sovereign, open-source, self-hosted event hub for personal multi-channel messaging (WhatsApp, Signal, SMS, Telegram, Discord). Messaging traffic lands in Matrix portal rooms via Mautrix bridges, is normalised by the Sensor into versioned CloudEvents on a NATS JetStream bus, and is consumed by Hermes-hosted personas under human oversight. See `README.md` for the full picture and `CONTEXT.md` for the domain vocabulary.

As of 2026-09-17 the repository contains documentation, the complete event contract, and the Sensor's integration-test harness — no component implementation yet:

- `README.md`, `docs/wireframes/companion-v0.1.md`, `docs/architecture/roadmap.md` (milestones ↔ lots), `docs/architecture/adr/` (ADRs 0005–0009 written; numbers 0001–0004 are referenced from the docs but not yet written)
- `contracts/cloudevents/v1/` — the complete v1 contract: 8 CloudEvents schemas plus one validated fixture per type
- `.scratch/sensor/` — local mirrors of the Sensor spec and tickets 01–11; **canonical is the tracker: GitHub Issues on [linagora/twalk](https://github.com/linagora/twalk/issues)** (conventions in `docs/agents/issue-tracker.md`); all 11 tickets are done and merged
- `sensor/` — the Sensor Cargo package: library modules (`config`, `consent`, `metrics`, `network`, `normalize`, `outbound` — pure logic) plus the binary wiring to matrix-sdk and NATS in `src/main.rs`; integration-test harness in `tests/` (compose stack, bot/bus/contract helpers, `crypto.rs` for Megolm-capable bots, `smoke.rs`, `sensor_lifecycle.rs`, `message_to_event.rs`, `consent.rs`, `reactions.rs`, `presence.rs`, `outbound.rs`, `persistence.rs`, `fidelity.rs`, `encryption.rs`, `observability.rs`, `deployment.rs`)
- `deploy/docker-compose/` — the reference deployment: Synapse + NATS JetStream + the Sensor wired together (`compose.yaml`, documented `.env.example`, `provision.sh` account provisioning, Synapse config template, Sensor Dockerfile), covered end-to-end by `sensor/tests/deployment.rs`
- Other component directories hold stub READMEs only: `hermes/`, `companion/`, `companion-gateway/`, `bridges/`, `ui/`, `sdk/`, `examples/`, `tools/`, `tests/`

## Build and test commands

Requires Docker (the harness brings up its own Synapse + NATS stack) and a Rust toolchain.

```bash
cd sensor
cargo build --tests   # compile / typecheck
cargo test            # full suite: boots the stack itself, ~15s cold, ~3s warm
```

## Code organization

Monorepo with 13 top-level directories — see the "Repository layout" section of `README.md`. Stacks: Rust for `sensor/` and `companion-gateway/`, SvelteKit (static export) for `companion/`. The Sensor test harness speaks the Matrix client-server API over plain HTTP — the HTTP `Bot` helpers deliberately do not depend on matrix-sdk; the exception is the `CryptoBot` helper (`sensor/tests/harness/crypto.rs`), which runs matrix-sdk with its crypto stack because raw HTTP cannot Megolm-encrypt.

## Code style guidelines

In all naming and prose, follow the `CONTEXT.md` vocabulary: **network** (never "channel" outside user-facing copy, never a bridge name like `gmessages`), **persona** (never "bot" for agent identities — test Matrix users standing in for bridges are "test bots"), **the Companion** means the PWA only, **Companion Gateway** always in full.

## Testing instructions

Tests live at the agreed seam: the Sensor process boundary (real Synapse + real NATS JetStream). Helpers are in `sensor/tests/harness/`; new test files go beside `smoke.rs`. Every event-producing behaviour must assert schema validity via `validate_against_contract`. Keep tests isolated from persisted stack state (unique rooms and bus subjects per run).

## Security considerations

Never commit secrets, credentials, or `.env` files (`.gitignore` covers `.env`). The only secrets in the repo are the throwaway constants in `sensor/tests/synapse/homeserver.yaml` and the test-bot passwords — local, ephemeral test stack only; never reuse them elsewhere. Message content is highly sensitive by design: no message may leave the user's infrastructure without explicit consent — keep that invariant in mind for any code that touches event payloads.

## Agent skills

### Issue tracker

GitHub Issues on [linagora/twalk](https://github.com/linagora/twalk/issues), with parent `spec` issues, native dependencies and one PR per ticket. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical roles, each label named after its role, plus this repo's own `spec`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` at the root, ADRs in `docs/architecture/adr/`. See `docs/agents/domain.md`.
