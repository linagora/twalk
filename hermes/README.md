# hermes

The agent platform: the consumer runtime plus the reference personas (`assistant` first, then `watch`, `archive`, `writing`, and a triage persona). Personas consume events from the bus, reason with a local or remote LLM, and publish suggestions or replies back on the bus.

The consumer runtime is written in Rust. Personas are always separate processes talking to the bus — never in-process — so first-party and third-party personas take the same path. The reference persona `assistant` is written in Python against the SDK (see `docs/architecture/adr/0008-hermes-rust-runtime-personas-as-processes.md`).
