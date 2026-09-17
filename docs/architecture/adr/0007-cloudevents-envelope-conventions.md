# CloudEvents envelope conventions: two families, per-producer source URIs, deterministic ids

Three conventions govern all v1 schemas.

1. **Two families.** Message-flow events (`inbound.*`, `persona.*`) require the `network` and `consent` extensions on every event. Operational events (`consent.state.changed`, `bridge.status.changed`) declare them optional: a bridge going down has no meaningful consent state, and a consent change can span several networks.
2. **Per-producer source URIs.** Sensor → `matrix://<homeserver>/<room_id>`, Hermes → `hermes://<domain>/personas/<persona_id>`, Companion Gateway → `gateway://<domain>/<resource>`.
3. **Deterministic ids.** Each schema documents a natural key hashed with sha256 (e.g. `matrix_event_id:room_id` for inbound events, `persona_id:trigger_event_id:attempt` for suggestions), so replaying or re-emitting the same underlying occurrence yields the same id and consumers can deduplicate. Random ids only where no natural key exists.

Rationale: a required attribute must mean something on every event, or consumers learn to ignore it; per-producer schemes keep sources filterable; deterministic ids are what make the bus safely replayable.
