# hermes

The agent platform: the consumer runtime plus the reference personas (`assistant` first, then `watch`, `archive`, `writing`, and a triage persona). Personas consume events from the bus, reason with a local or remote LLM, and publish suggestions or replies back on the bus.

The consumer runtime is written in Rust. Personas are always separate processes talking to the bus — never in-process — so first-party and third-party personas take the same path. The reference persona `assistant` is written in Python against the SDK (see `docs/architecture/adr/0008-hermes-rust-runtime-personas-as-processes.md`).

## Status

The `twalk-hermes` Cargo package exists but the runtime is not implemented yet. What is here is the suite that the rest of the Hermes work is built on: `tests/smoke.rs` exercises the shared test harness (`tests/harness/`) against the real stack — the stack boots, the bus round-trips a persona event with its deterministic id, the stub LLM answers persona tests the same way every run, and contract validation catches an invalid event.

```bash
cd hermes
cargo test   # boots the shared test stack itself (Docker required)
```

The stack is the same one the Sensor suite uses, so the two suites share one Synapse and one NATS JetStream; `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT` and `TWALK_TEST_NATS_PORT` move it aside for parallel worktrees.
