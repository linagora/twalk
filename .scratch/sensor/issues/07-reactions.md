# 07: Reactions

**GitHub:** [linagora/twalk#8](https://github.com/linagora/twalk/issues/8) — canonical on the tracker; this file is the local mirror.

**What to build:** a 👍 on a message becomes a first-class event. When a bot adds a reaction in an observed room, a schema-valid `inbound.reaction.added.v1` appears on the bus carrying the reaction itself, the target message's event id with an excerpt, and the resolved contact. Reaction removals produce no v1 event, per the contract.

**Blocked by:** 02 — Walking skeleton.

**Status:** ready-for-agent

- [ ] A reaction in an observed room produces a schema-valid `inbound.reaction.added.v1`
- [ ] The event carries the reaction, the target event id and an excerpt of the target message
- [ ] The reactor is resolved to a contact with the same rules as message senders
- [ ] Removing a reaction produces no event
