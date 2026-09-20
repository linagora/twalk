# The bus has a retention policy (#174) — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `twalk` JetStream stream's retention becomes an explicit, reviewable, operator-set policy — 90 days, 2 GiB, `discard old`, `s2` compression, a 24-hour duplicate window — reconciled onto an existing stream at the Sensor's startup, recorded in an ADR that says out loud that the bus holds contacts' messages.

**Architecture:** Five components `get_or_create_stream` with `..Default::default()` today (sensor, hermes, gateway ×2, clerk); the stream's **policy** gets one owner, the Sensor (`sensor/src/bus.rs`, new, pure: `StreamPolicy` from the environment → a `stream::Config` with every field named; `ensure_stream` creates it or **updates an existing one in place**, logging each field that changed, and refuses loudly — never recreating — when the update is rejected). The other four keep creating a bare stream if none exists (a deployment that starts the Gateway first still works) and are unchanged. `.env.example` and compose pass the three knobs through.

**Tech Stack:** Rust, async-nats 0.50 (`jetstream::stream::Config`, `get_stream`, `update_stream`, `Compression::S2`, `DiscardPolicy::Old`).

**Spec:** GitHub issue #174 (mirror `.scratch/sensor/ticket-174.md`) — the decision is the operator's, taken 2026-09-19.

## Global Constraints

- Values: `SENSOR_BUS_MAX_AGE_DAYS=90`, `SENSOR_BUS_MAX_BYTES=2147483648`, `SENSOR_BUS_DUPLICATE_WINDOW_SECONDS=86400`; `discard: Old`; `compression: S2`; `storage: File`; `num_replicas: 1`; every other field of `stream::Config` named explicitly in the code (no `..Default::default()`), so a reader sees the policy without knowing NATS's defaults.
- An existing stream is **updated in place**; what changed is logged field by field (`old → new`); a rejected update is an `error` naming the field and the operator's options (the stream is never deleted or recreated by Twalk — expired events are gone for good, and that is the operator's act, not a restart's).
- Tests at the process boundary: after the Sensor starts, the bus's own `stream info` shows the policy; a stream pre-created with defaults is updated; a `Nats-Msg-Id` published twice is one message. The 24-hour window is asserted from the bus's config (the two-minute wait the ticket sketches is not a test this suite runs — say so in the test's doc).
- ADR 0037 (`docs/architecture/adr/0037-the-bus-keeps-ninety-days-and-two-gigabytes-and-no-more.md`): hard to reverse, surprising, a trade-off against replay and ADR 0012/0028; **states that the bus holds contacts' messages and that retention is therefore a personal-data decision**; states what a Gateway installed fresh after 90 days cannot rebuild; states the duplicate window as a correctness fix (a Sensor re-sync republishes past two minutes).
- Vocabulary; `cargo fmt` only; docs: `.env.example` (documented block), `compose.yaml` passthrough, `sensor/README.md` if it lists variables, `AGENTS.md` one sentence, `docs/architecture/security-model.md` if it has a retention/residual-risk line for the bus.

### Task 1: the policy, the reconciliation, the tests
Files: `sensor/src/bus.rs` (new), `sensor/src/lib.rs`, `sensor/src/config.rs` (the three variables, validated ≥ 1), `sensor/src/main.rs` (call `bus::ensure_stream`), `sensor/tests/bus_policy.rs` (new), `.github/ci/suites.json` (the new test file in the sensor suite's targets). Unit tests in `bus.rs`: the config builder names the values; `diff(existing, wanted)` lists exactly the changed fields. Process-boundary: the three assertions above, on a stream name of the run's own (the harness's `Bus` can name one — do not touch the shared `twalk` test stream).
### Task 2: ADR 0037, `.env.example`, compose, docs, mirror.
