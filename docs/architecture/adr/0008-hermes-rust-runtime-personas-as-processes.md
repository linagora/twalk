# Hermes: Rust runtime, personas as separate processes

Hermes's consumer runtime is written in Rust, consistent with the Sensor and the Companion Gateway. Personas never run in-process: each persona is a separate process talking to the bus, exactly like a third-party author's persona will be. The reference persona `assistant` is written in Python against the `twalk-sdk`, proving the SDK path from day one.

Rationale: one runtime to operate, no privilege gap between first-party and third-party personas, and Python's LLM ergonomics where they matter — inside personas — without infecting the runtime.
