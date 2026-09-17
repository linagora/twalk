---
component: hermes
status: ready
labels: [spec]
---

# Hermes — Spec

## Problem Statement

Messages now flow onto the bus as typed, schema-valid events — and nothing answers. The operator's promise is an assistant that reads incoming messages and suggests replies under their control; today there is no runtime to host such a persona, no SDK to write one with, and no path for an approval to become an outbound reply. Without Hermes, the bus is a one-way tape.

## Solution

Hermes is the agent platform, in two halves. A **runtime** (Rust) hosts personas: it starts and supervises one process per activated persona, and it exposes the approval endpoint that turns a human decision into a `persona.reply.approved` event. A **persona SDK** (Python first) lets a persona author consume inbound events and produce thinking/suggestion events in a few lines, with the consent gate enforced by the SDK itself. The reference persona **assistant** is built on it: it reads inbound messages, asks an LLM (local or remote, operator-chosen), and produces a suggestion — never a send.

The operator's experience: activate `assistant`, watch thinking and suggestions appear in the oversight interface, approve one, and see it land back in the conversation through the Sensor.

## User Stories

Persona authors:

1. As a persona author, I want a Python SDK that subscribes me to inbound events, so that I never touch NATS mechanics.
2. As a persona author, I want the SDK to drop events whose consent is not `granted` before my code sees them, so that I cannot breach consent by mistake.
3. As a persona author, I want helpers that build contract-valid `persona.thinking.emitted` / `persona.suggest.produced` envelopes, so that my persona cannot emit malformed events.
4. As a persona author, I want deterministic ids computed for me from the contract's natural keys, so that my replays deduplicate safely.
5. As a persona author, I want the trigger reference (event id, type) and the traceparent carried through automatically, so that oversight can link my events to the trigger.
6. As a persona author, I want to talk to any OpenAI-compatible chat-completions endpoint via configuration, so that I can develop against a local model and deploy against a remote one.

Operators:

7. As the operator, I want the runtime to start exactly the personas I activated, so that the system matches my configuration.
8. As the operator, I want a crashed persona restarted with backoff, so that one bug does not silence my assistant.
9. As the operator, I want persona lifecycle (started, crashed, restarted, stopped) visible in structured logs, so that I can see what is running.
10. As the operator, I want to deactivate a persona without restarting the runtime.
11. As the operator, I want the LLM endpoint and its credentials configurable by environment, so that no message content leaves my infrastructure unless I chose the endpoint.
12. As the operator, I want everything configured through environment variables, so that Hermes joins the compose deployment like the Sensor did.

Users (oversight):

13. As the user, I want a `thinking` event when the assistant starts on my message, so that the UI can show activity in real time.
14. As the user, I want a `suggest` event carrying the proposed reply, so that I can review before anything is sent.
15. As the user, I want approval to be a deliberate act — an explicit API call, so that nothing sends without me.
16. As the user, I want the approval to carry my identity (`approved_by`), so that the audit trail says who sent what.
17. As the user, I want an approval for a revoked-consent contact rejected, so that consent is enforced at the last gate too.
18. As the user, I want the approved event to carry the final (possibly edited) content, so that what I approve is exactly what goes out.
19. As the user, I want a suggestion that can expire, so that a stale suggestion cannot be approved late.

Contract consistency:

20. As a downstream consumer, I want persona events schema-valid with deterministic ids and `NATS-Msg-Id` set, so that replay and dedup behave exactly like the inbound path.
21. As a downstream consumer, I want `traceparent` continued from the trigger event, so that a message's trace links sensor → persona → approval → outbound.
22. As the bus operator, I want persona subscriptions durable, so that a persona restart does not lose events.

End to end:

23. As the user, I want the full loop — message in, thinking, suggestion, my approval, reply out — so that Twalk answers on my behalf under my control.

## Implementation Decisions

- **Two halves per ADR 0008**: the runtime (`hermes/`, Rust) hosts personas as separate processes; personas talk to the bus themselves via the SDK, never in-process. The reference persona `assistant` is Python, in `hermes/personas/assistant/`, built on `sdk/python/twalk_sdk`.
- **Consent enforcement is consumer-side**: JetStream cannot filter by message headers, so the SDK drops non-`granted` events before persona code runs (unit-tested gate, impossible for an author to forget). The runtime re-checks consent at approval time (defense in depth): an approval whose suggestion's trigger was not `granted` is rejected.
- **Runtime duties**: read persona configuration (env-provided list in v0.1: ids, commands, activation); spawn and supervise persona processes (restart with exponential backoff, structured logs per lifecycle transition); expose the approval HTTP API; ensure its consumers/streams exist at startup.
- **Approval API**: `POST /v1/approvals` on the runtime (loopback-only by default), body `{suggestion_event_id, approved_by, final?}`. The runtime validates: the suggestion exists on the bus, its consent was `granted`, it is not expired (`expires_at`); then publishes `persona.reply.approved.v1` with deterministic id `sha256(suggestion_event_id:approved_by)`, final content = edited body if provided else the suggestion's body, `edited` flag accordingly. The Sensor's existing outbound path posts it.
- **Assistant persona**: consumes `twalk.inbound.message.received.v1` (durable consumer, ack after processing), publishes `thinking` (id from the contract natural key), calls the LLM with the message plus a small system prompt, publishes `suggest` (attempt counter, `expires_at` set by policy — default 1 hour). Non-text or attachment-only messages are skipped quietly for v0.1.
- **Deterministic ids** per the contract: thinking = `sha256(persona_id:trigger_event_id)`, suggest = `sha256(persona_id:trigger_event_id:attempt)`, reply.approved = `sha256(suggestion_event_id:approved_by)`. `NATS-Msg-Id` set on every publish; `network`/`consent`/`traceparent` duplicated as NATS headers (traceparent continued from the trigger when present).
- **Persona packaging**: personas ship as container images (the SDK and its deps install inside; the host has no pip), which also matches the "separate processes" deployment story. The runtime spawns them as child processes in tests and as compose services in deployment.
- **Test harness sharing**: the reusable harness pieces (compose stack, Bus, contract validation) extract into a shared crate `tests/harness/` used by both `sensor/` and `hermes/` suites; Sensor-specific helpers (Bot, SensorProc) stay in `sensor/`.

## Testing Decisions

- **What makes a good test**: external behaviour only, at the process boundary — the same seam discipline as the Sensor.
- **Seam 1 (persona + SDK)**: drive the real `assistant` process by publishing contract fixtures to the bus; the LLM is a stub HTTP server returning a canned completion. Assert `thinking` then `suggest` appear, schema-valid, with the right trigger reference, deterministic id, and continued traceparent. The consent gate: a `pending`/`revoked` inbound event produces neither event nor LLM call (assert the stub received nothing).
- **Seam 2 (runtime)**: start the runtime with a persona config; assert the persona process comes up and consumes; kill the persona process; assert the runtime restarts it (observable via a second `thinking` on the next message). Approval path: publish a suggest fixture, POST an approval, assert a schema-valid `persona.reply.approved.v1` on the bus; approve a revoked-consent suggestion → HTTP 4xx and no event; approve an expired suggestion → 4xx.
- **Prior art**: the Sensor suite's patterns (fixture patching with per-run unique ids, `poll_until`, schema validation, `SENSOR_LOCK`-style serialization) carry over through the shared harness crate.
- **Full loop**: one end-to-end test ties Sensor + runtime + assistant on the same stack: bot message → thinking → suggest → approval POST → reply visible in the Matrix room.

## Out of Scope

- The other reference personas (`watch`, `archive`, `writing`, triage) — v0.2.
- The Companion Gateway (the approval API's future caller) and Buzz's Dialogue Room UI.
- Persona configuration UI, hot reload, multi-instance/high-availability.
- Tool calling, multi-turn planning, streaming suggestions — v0.1 is single-completion.
- TypeScript and Rust SDKs (v0.2); third-party packaging/distribution of the Python SDK.
- The `matrix` network value and attachment `encryption` object land in parallel tickets (#17/#18); Hermes code must tolerate unknown network values forward-compatibly regardless.

## Further Notes

- The environment has no host-side pip (PEP 668) and no venv module: Python dependencies live inside the persona's container image; tests orchestrate it via compose. This is a constraint discovered during the Sensor build, now a deliberate packaging decision.
- The approval API is the v0.1 minimum that makes the loop real; when the Companion Gateway exists it becomes the API's client, and the Buzz Dialogue Room drives it from the UI.
- A persona that misbehaves can only pollute `persona.*` subjects it is scoped to; the runtime's supervision and the bus's auditability are the containment story.
