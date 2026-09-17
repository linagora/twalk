# 08: Presence

**GitHub:** [linagora/twalk#9](https://github.com/linagora/twalk/issues/9) — canonical on the tracker; this file is the local mirror.

**What to build:** personas can tell when a contact is around. Presence updates of bridge puppets sharing observed portal rooms are published as schema-valid `inbound.presence.updated.v1` events (online, offline, unavailable), with the deterministic id derived from the documented natural key — presence updates carry no Matrix event id. Presence is best-effort: a bridge or network that provides no presence must not break or slow the rest of the pipeline.

**Blocked by:** 02 — Walking skeleton.

**Status:** done

- [x] A puppet presence change produces a schema-valid `inbound.presence.updated.v1`
- [x] The event id follows the deterministic natural key defined in the schema
- [x] A bridge without presence support leaves the rest of the pipeline unaffected
