---
component: sensor
status: ready
labels: [ready-for-agent]
---

# Sensor — Spec

## Problem Statement

A Twalk operator's messages already land in Matrix portal rooms through the Mautrix bridges — but they sit there, encrypted and invisible. Nothing turns them into typed, replayable events on the bus. Without the Sensor there is no inbound path at all: personas cannot observe anything, oversight interfaces show nothing, and approved replies have no way back out to the networks. The Sensor is the single chokepoint of the entire inbound pipeline.

## Solution

The Sensor is a Matrix client that lives on the operator's homeserver. Invited into a portal room, it joins, decrypts the room's events end-to-end, enriches them with contact and consent context, and publishes them to the bus as CloudEvents conforming to the v1 contract — exactly once per underlying occurrence, however often it resynchronizes. It also closes the loop: when a `persona.reply.approved` event appears on the bus, the Sensor posts the approved reply into the target portal room, and the bridge carries it back to the external network.

The operator's experience: bridges connected, Sensor running, and the event stream simply flows — observable in the Buzz Control Room, replayable from the bus, auditable forever.

## User Stories

Inbound — messages:

1. As a persona author, I want every inbound message delivered as a schema-valid `inbound.message.received.v1` CloudEvent, so that I can reason about messages without any Matrix knowledge.
2. As a persona, I want the sender resolved to a structured `contact` (display name, network identifier when consent allows), so that I can address people naturally.
3. As a persona, I want each event labelled with its `network`, so that I can filter WhatsApp from SMS without parsing payloads.
4. As a persona, I want each event labelled with the sender's current `consent` state, so that I can refuse processing when consent is not `granted`.
5. As the operator, I want message attachments carried as `mxc://` references with kind, MIME type and size, so that no binary ever transits the bus.
6. As a persona, I want reply relationships preserved (`reply_to` with an excerpt of the parent), so that I can reason about context without a bus lookup.
7. As a persona, I want thread roots preserved when the network supports threads.
8. As the operator, I want the original network timestamp preserved alongside the Sensor's own production timestamp, so that the audit trail reflects what really happened.
9. As a persona, I want message bodies capped and formatted per the contract (`text/plain` by default), so that I can trust payload shapes.

Inbound — reactions and presence:

10. As a persona, I want reactions delivered as `inbound.reaction.added.v1` events with the target message reference and an excerpt, so that I can treat a 👍 as an answer without fetching history.
11. As a persona, I want presence changes delivered as `inbound.presence.updated.v1` events, so that I know when a contact is around before suggesting a reply.
12. As the operator, I want presence handled best-effort per bridge capability, so that networks without presence don't break the pipeline.

Delivery guarantees:

13. As the operator, I want every event id derived deterministically from its natural key, so that replaying or resynchronizing never produces duplicates downstream.
14. As the operator, I want the Sensor to survive restarts without re-publishing already-seen Matrix events, so that the bus stays clean across deployments.
15. As the operator, I want JetStream deduplication keyed on the CloudEvents id, so that even a double publish is absorbed by the bus.
16. As a downstream consumer, I want at-least-once delivery end-to-end, so that no message is ever silently lost.
17. As the operator, I want the Sensor to resume from a persisted sync token after a disconnect, so that nothing is missed while it was down.

Encryption and identity:

18. As the operator, I want the Sensor to decrypt Megolm-encrypted portal rooms end-to-end, so that message content never exists in plaintext on the homeserver outside the Sensor.
19. As the operator, I want the Sensor to bootstrap its cryptographic identity from the recovery key I saved during onboarding, so that I can replace the Sensor device without losing history.
20. As the operator, I want the Sensor to cross-sign bridge puppet devices it shares portal rooms with as far as the bridges allow, so that decryption works without manual verification of every puppet.
21. As the operator, I want decryption failures surfaced (logged, counted, and the raw event skipped without crashing the pipeline), so that one broken room never stalls the others.

Scope of observation:

22. As the operator, I want the Sensor to observe exactly the rooms it is invited to — invitation being the act of consent to observation — so that no room is ever watched by default.
23. As the operator, I want the Sensor to accept invitations only from known bridge provisioning users or my own account, so that a random invite can't feed third-party content into my bus.
24. As the operator, I want leaving a room (or the bridge decommissioning it) to stop observation of that room cleanly.

Outbound:

25. As the operator, I want an approved reply (`persona.reply.approved.v1`) posted to the target portal room as a native reply to the original message, so that the contact sees a normal conversation.
26. As the operator, I want the final (possibly edited) content sent, never the raw suggestion, so that what I approved is exactly what goes out.
27. As the operator, I want the outbound send to flow back through the bridge as a normal inbound echo, so that the audit trail closes the loop naturally without a special "sent" event.
28. As the operator, I want send failures retried with backoff and surfaced after exhaustion, so that an approved reply is never silently dropped.

Operations:

29. As the operator, I want the Sensor configured entirely through environment variables (homeserver, credentials, recovery key, NATS URL), so that it deploys with the rest of the compose stack.
30. As the operator, I want structured logs and basic metrics (events published, decryption failures, lag), so that the dashboard can show Sensor health.
31. As the operator, I want distributed tracing propagated via `traceparent` when present, so that I can follow a message from portal room to persona.

## Implementation Decisions

- **Language and runtime**: Rust, Tokio. One long-lived process per deployment.
- **Matrix stack**: `matrix-sdk` with the `e2ee` feature. Megolm, device management, key backup/restore and cross-signing are delegated to `matrix-sdk-crypto` — the Sensor never implements cryptography itself. (This decision de facto defines the boundary ADR 0003 will document.)
- **Observation scope is invitation-driven**: the Sensor auto-joins rooms when the inviter is a configured bridge provisioning user or the operator's own account; it ignores all other invitations. No room is observed by default.
- **Normalization** is a pure function from (decrypted Matrix event, room state, consent cache) to a CloudEvents envelope per `contracts/cloudevents/v1/`. All contract rules apply: deterministic ids from natural keys, per-producer `source` URIs (`matrix://<homeserver>/<room_id>`), required `network`/`consent` extensions (ADR 0007), reverse-DNS types.
- **Publishing**: JetStream, structured mode — the full CloudEvents JSON envelope as the message body — with `network`, `consent` and `traceparent` duplicated as NATS headers for server-side filtering without deserialization. `NATS-Msg-Id` set to the CloudEvents id, giving JetStream-window deduplication on top of deterministic ids.
- **Subject naming**: `twalk.<domain>.<action>.<version>` (e.g. `twalk.inbound.message.received.v1`), matching the wildcard subscriptions the schemas describe.
- **Consent labelling**: the Sensor maintains a local consent cache, fed by consuming `consent.state.changed.v1` events from the bus plus an initial fetch from the Companion Gateway API at startup. The Sensor only labels events; personas gate processing (ADR 0006).
- **Contact enrichment** resolves the bridge puppet's profile from room membership state and bridge metadata; `network_identifier` is populated only when the sender's consent state allows it (per the contract).
- **Presence** is observed from `/sync` presence updates for bridge puppets sharing observed portal rooms, best-effort per bridge.
- **Replies outbound**: a durable JetStream consumer on `twalk.persona.reply.approved.v1`; the Sensor posts an `m.room.message` with an `m.in_reply_to` relation into `target.room_id`. Exponential backoff retry; after exhaustion the event goes to a dead-letter subject and an error is logged — no silent drops.
- **Sync state persistence**: the sync token and the crypto store are persisted on disk (or a named volume), so restarts resume rather than replay.
- **Configuration**: environment variables only — homeserver URL, Matrix user ID + access token, recovery key, NATS URL and credentials, allowed inviters, log level.
- **No schema drift**: the build vendored or referenced `contracts/cloudevents/v1/` as the single source of truth; every published event must validate.

## Testing Decisions

- **What makes a good test here**: external behaviour only. A test drives Matrix traffic into the Sensor's rooms and asserts on what appears on the bus (or drives bus traffic and asserts on what appears in Matrix). It never reaches inside the process.
- **One seam, the process boundary**: integration tests run the real Sensor binary against a real Synapse and a real NATS JetStream (docker-compose, the same stack `deploy/` targets). Test-side Matrix bots play the role of bridges: they create portal-shaped rooms (encrypted and unencrypted variants), invite the Sensor, and send messages, reactions and presence updates. Assertions consume `twalk.*` subjects and validate every received event against the v1 JSON Schemas — the fixtures in `contracts/cloudevents/v1/fixtures/` are the reference shapes.
- **Outbound direction**: the test publishes the `persona.reply.approved` fixture to JetStream and asserts the message appears in the Matrix room with the correct reply relation and the final (edited) content.
- **Replay/idempotency tests**: restart the Sensor mid-scenario and re-run bridge traffic; assert no duplicate ids appear on the bus.
- **Unit tests** only for pure decisions: id derivation from natural keys, subject mapping, envelope construction.
- **Prior art**: none in this repo yet — these are the first tests. Matrix-side idioms follow `matrix-sdk` examples; compose-based integration testing follows the pattern `deploy/docker-compose` will standardize.

## Out of Scope

- Bridge provisioning, registration and configuration (`bridges/`, `deploy/`) — tests simulate bridges with bots.
- The Companion Gateway and its consent API — the Sensor consumes consent state but the writer is specified separately (ADR 0006).
- Hermes, personas, and any agent reasoning.
- The Companion PWA, Buzz, Messagr.
- Reaction *removals*, message edits and redactions (no v1 event types exist for them yet).
- Media re-hosting or thumbnail generation — attachments stay `mxc://` references.
- Telegram and Discord specifics (v0.2), email/PIM via Pimalaya (later), RCS (no public API).
- Multi-Sensor high availability — exactly one Sensor process per deployment for now.
- The SMS Companion Android app (v0.2).

## Further Notes

- ADR 0003 (`sensor-encryption-boundary`) is referenced by the wireframes but not yet written; the crypto-boundary decisions here are its substance — backfill the ADR when the implementation lands.
- The recovery-key bootstrap flow (user story 19) must interoperate with what the Companion screen 2 generates; coordinate the format when the Companion is specified.
- Pin `matrix-sdk` conservatively; the e2ee API surface evolves quickly.
