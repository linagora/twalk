# 05: Contact resolution and consent labelling

**GitHub:** [linagora/twalk#6](https://github.com/linagora/twalk/issues/6) — canonical on the tracker; this file is the local mirror.

**What to build:** events tell personas who is speaking and whether they may be processed. The sender is resolved from room membership and bridge metadata into the contract's `contact` shape. The Sensor maintains a consent cache fed by consuming `consent.state.changed.v1` events from the bus; every published event carries the sender's current consent state, and a change published on the bus is reflected in subsequent events. Unknown senders default to `pending`. `network_identifier` is only populated when the sender's consent state allows it. The initial consent fetch sits behind an interface with a no-op implementation until the Companion Gateway exists — the Sensor never writes consent state, it only labels (ADR 0006).

**Blocked by:** 02 — Walking skeleton.

**Status:** ready-for-agent

- [ ] Events carry a resolved `contact.display_name` from membership and bridge metadata
- [ ] `network_identifier` appears only when the sender's consent state allows it
- [ ] A `consent.state.changed` event on the bus changes the label on that sender's subsequent events
- [ ] Unknown senders are labelled `pending`
- [ ] The initial consent fetch is behind an interface with a no-op default; the Sensor never writes consent state
