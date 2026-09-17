//! Hermes: the agent platform's consumer runtime.
//!
//! The runtime hosts personas as separate processes, supervises them, and
//! exposes the approval endpoint that turns a human decision into a
//! `persona.reply.approved` event (see
//! `docs/architecture/adr/0008-hermes-rust-runtime-personas-as-processes.md`).
//!
//! Nothing is implemented yet: ticket #20 creates this package so that the
//! Hermes suite can exercise the shared test harness against the real stack
//! before the runtime exists — the same order the Sensor was built in. The
//! runtime lands in the tickets that follow.
