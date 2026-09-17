# AGENTS.md

## Project overview

`twalk` is the monorepo for Twalk, a sovereign, open-source, self-hosted event hub for personal multi-channel messaging (WhatsApp, Signal, SMS, Telegram, Discord). Messaging traffic lands in Matrix portal rooms via Mautrix bridges, is normalised by the Sensor into versioned CloudEvents on a NATS JetStream bus, and is consumed by Hermes-hosted personas under human oversight. See `README.md` for the full picture and `CONTEXT.md` for the domain vocabulary.

As of 2026-09-17 the repository contains documentation, the directory scaffold and the complete event contract — no source code yet:

- `README.md`, `docs/wireframes/companion-v0.1.md`, `docs/architecture/adr/` (ADRs 0005–0008 written; numbers 0001–0004 are referenced from the docs but not yet written)
- `contracts/cloudevents/v1/` — the complete v1 contract: 8 CloudEvents schemas plus one validated fixture per type
- `.scratch/sensor/spec.md` — the Sensor spec, labelled `ready-for-agent` (local tracker convention until `/setup-matt-pocock-skills` is run)
- Empty component directories with stub READMEs: `sensor/`, `hermes/`, `companion/`, `companion-gateway/`, `bridges/`, `deploy/`, `ui/`, `sdk/`, `examples/`, `tools/`, `tests/`

## Build and test commands

None yet. No build system or package manifest exists (no `Cargo.toml`, `package.json`, etc.).

## Code organization

Monorepo with 13 top-level directories — see the "Repository layout" section of `README.md`. Planned stacks per the docs: Rust for `sensor/` and `companion-gateway/`, SvelteKit (static export) for `companion/`.

## Code style guidelines

No code yet. In all naming and prose, follow the `CONTEXT.md` vocabulary: **network** (never "channel" outside user-facing copy, never a bridge name like `gmessages`), **persona** (never "bot"), **the Companion** means the PWA only, **Companion Gateway** always in full.

## Testing instructions

No test framework or suite yet. `tests/` is reserved for end-to-end and CloudEvents conformance tests.

## Security considerations

Never commit secrets, credentials, or `.env` files. Message content is highly sensitive by design: no message may leave the user's infrastructure without explicit consent — keep that invariant in mind for any code that touches event payloads.
