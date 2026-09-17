# 01: Integration test harness

**What to build:** the testing seam the whole component will be verified through. A single command brings up a real Synapse and a real NATS JetStream for tests, with Matrix bot users provisioned to play the role of bridges. Test helpers cover the actions every later ticket needs: create a room (encrypted or not), invite a user, send a message, a reaction, a presence update, consume a bus subject, and assert that a received event validates against a contract schema. This ticket contains no Sensor code at all — it delivers the instrument, and proves it with a smoke test: a bot posts to Synapse, a NATS round-trip works, and every fixture in the contract validates against its schema. If Synapse proves too heavy to boot in CI, a lighter Matrix homeserver is an acceptable fallback as long as the client-server API surface used by the tests stays identical.

**Blocked by:** None (can start immediately).

**Status:** done

- [x] One command brings up Synapse and NATS JetStream for tests, with bot users provisioned
- [x] Helpers exist for: create room (encrypted or unencrypted), invite, send message, send reaction, set presence, consume a subject, validate an event against a contract schema
- [x] A smoke test passes using only the harness: bot posts to Synapse, NATS round-trip works
- [x] Every contract fixture validates against its schema in the harness
- [x] No Sensor implementation code exists in this ticket
