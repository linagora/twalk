# 02: Walking skeleton — message in, event out

**GitHub:** [linagora/twalk#3](https://github.com/linagora/twalk/issues/3) — canonical on the tracker; this file is the local mirror.

**What to build:** the thinnest complete path through the Sensor. An operator starts the process from environment configuration alone (homeserver, credentials, NATS URL, allowed inviters). The Sensor auto-joins a room when the inviter is allowed (a known bridge provisioning user or the operator's own account), ignores every other invitation, and stops observing a room after leaving it. When a bot sends a plain text message in an unencrypted room, a schema-valid `inbound.message.received.v1` CloudEvent appears on the bus: deterministic id, `matrix://` source URI, `network` extension, and a consent label defaulting to `pending` for unknown senders. Publishing sets `NATS-Msg-Id` to the CloudEvents id so JetStream deduplication works from day one.

**Blocked by:** 01 — Integration test harness.

**Status:** done

- [x] The Sensor runs from environment configuration alone
- [x] It auto-joins on invitation from an allowed inviter and ignores other invitations
- [x] It stops observing a room after leaving or being removed from it
- [x] A plain text message from a bot in an unencrypted room produces a schema-valid `inbound.message.received.v1` on subject `twalk.inbound.message.received.v1`
- [x] The event id is deterministic from its natural key; `NATS-Msg-Id` equals the event id
- [x] Unknown senders are labelled with consent `pending`
- [x] Unit tests cover id derivation and subject mapping
