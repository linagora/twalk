# 09: Outbound — approved replies

**GitHub:** [linagora/twalk#10](https://github.com/linagora/twalk/issues/10) — canonical on the tracker; this file is the local mirror.

**What to build:** the loop closes. The Sensor durably consumes `twalk.persona.reply.approved.v1` and posts the approved reply into the target portal room as a native reply to the original message — the final, possibly edited, content, never the raw suggestion. The bridge then carries the message to the external network, and its echo flows back through the Sensor as a normal inbound event: the audit trail closes without any special "sent" event. Failed sends retry with exponential backoff; after exhaustion the event moves to a dead-letter subject and the error is logged — an approved reply is never silently dropped.

**Blocked by:** 02 — Walking skeleton.

**Status:** done

- [x] Publishing the `persona.reply.approved` fixture makes the message appear in the target Matrix room
- [x] The posted content is the final content, with an `m.in_reply_to` relation to the original message
- [x] Failed sends retry with exponential backoff
- [x] After retry exhaustion the event lands on a dead-letter subject and the failure is logged
- [x] The bridge echo of a sent message flows back as a normal inbound event
