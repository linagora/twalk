# 02: Walking skeleton — message in, event out

**What to build:** the thinnest complete path through the Sensor. An operator starts the process from environment configuration alone (homeserver, credentials, NATS URL, allowed inviters). The Sensor auto-joins a room when the inviter is allowed (a known bridge provisioning user or the operator's own account), ignores every other invitation, and stops observing a room after leaving it. When a bot sends a plain text message in an unencrypted room, a schema-valid `inbound.message.received.v1` CloudEvent appears on the bus: deterministic id, `matrix://` source URI, `network` extension, and a consent label defaulting to `pending` for unknown senders. Publishing sets `NATS-Msg-Id` to the CloudEvents id so JetStream deduplication works from day one.

**Blocked by:** 01 — Integration test harness.

**Status:** ready-for-agent

- [ ] The Sensor runs from environment configuration alone
- [ ] It auto-joins on invitation from an allowed inviter and ignores other invitations
- [ ] It stops observing a room after leaving or being removed from it
- [ ] A plain text message from a bot in an unencrypted room produces a schema-valid `inbound.message.received.v1` on subject `twalk.inbound.message.received.v1`
- [ ] The event id is deterministic from its natural key; `NATS-Msg-Id` equals the event id
- [ ] Unknown senders are labelled with consent `pending`
- [ ] Unit tests cover id derivation and subject mapping
