# hermes

The agent platform: the consumer runtime plus the reference personas (`assistant` first, then `watch`, `archive`, `writing`, and a triage persona). Personas consume events from the bus, reason with a local or remote LLM, and publish suggestions or replies back on the bus.

The consumer runtime is written in Rust. Personas are always separate processes talking to the bus — never in-process — so first-party and third-party personas take the same path. The reference persona `assistant` is written in Python against the SDK (see `docs/architecture/adr/0008-hermes-rust-runtime-personas-as-processes.md`).

## Status

The runtime is not implemented yet. What exists is the persona half:

- `personas/assistant/` — the reference persona ([#21](https://github.com/linagora/twalk/issues/21)): it consumes `inbound.message.received`, asks an LLM for a draft reply, and publishes `persona.thinking.emitted` then `persona.suggest.produced`. It ships as a container image and is built on `sdk/python/twalk_sdk`, which holds everything that must not be got wrong — starting with the consent gate, so that a persona author cannot breach consent by forgetting.
- `tests/assistant.rs` — the spec's seam 1: the real persona container, driven by publishing contract fixtures to the bus, with the stub LLM answering a canned completion. It asserts the two events are schema-valid, carry the contract's deterministic ids (`sha256(persona_id:trigger_event_id)` and `sha256(persona_id:trigger_event_id:attempt)`) with `Nats-Msg-Id` set, duplicate `network`/`consent`/`traceparent` as NATS headers and continue the trigger's trace — and it asserts the consent gate by absence: a `pending` message and a revoked sender's reduced message produce no event and no LLM call.
- `tests/smoke.rs` — the shared test harness ([#20](https://github.com/linagora/twalk/issues/20)) against the real stack: the stack boots, the bus round-trips a persona event with its deterministic id, the stub LLM answers the same way every run, and contract validation catches an invalid event.
- `tests/harness/` — the Hermes-specific half of the harness: `PersonaRun`, which brings up the persona image against the shared stack's bus with a stub LLM behind it, on a bus namespace of its own run.

Still to come: the runtime that starts and supervises persona processes ([#23](https://github.com/linagora/twalk/issues/23)), the approval API ([#24](https://github.com/linagora/twalk/issues/24)), and the suggestion policy the SDK's envelope is built to extend ([#22](https://github.com/linagora/twalk/issues/22)).

```bash
cd hermes
cargo test   # boots the shared test stack and builds the persona image itself (Docker required)
```

The stack is the same one the Sensor suite uses, so the two suites share one Synapse and one NATS JetStream; `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT` and `TWALK_TEST_NATS_PORT` move it aside for parallel worktrees. Each persona test runs the persona under its own compose project and on its own JetStream stream, and removes both afterwards.
