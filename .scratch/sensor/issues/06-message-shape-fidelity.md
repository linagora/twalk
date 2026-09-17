# 06: Message shape fidelity

**GitHub:** [linagora/twalk#7](https://github.com/linagora/twalk/issues/7) — canonical on the tracker; this file is the local mirror.

**What to build:** rich messages arrive contract-complete, so personas never need a bus lookup to reason about them. A reply produces `reply_to` with the parent's event id and an excerpt. Participation in a thread produces `thread_root`. An image or file produces an attachment entry — kind, `mxc://` URI, MIME type, size, optional caption and dimensions — with no binary ever transiting the bus. The original network timestamp is preserved alongside the Sensor's production timestamp.

**Blocked by:** 02 — Walking skeleton.

**Status:** ready-for-agent

- [ ] A reply message produces `reply_to` with parent event id and excerpt
- [ ] A threaded message produces `thread_root`
- [ ] An image message produces a schema-valid attachment entry with no binary on the bus
- [ ] `network_timestamp` is preserved when the bridge provides it
- [ ] Every produced event validates against the contract schema
