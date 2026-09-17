# Twalk

Twalk is a sovereign, self-hosted event hub that turns fragmented personal messaging (WhatsApp, Signal, SMS, Telegram, Discord) into a single auditable stream of typed CloudEvents that agents observe, reason about, and respond to under human control.

## Language

### Collection and normalisation

**Bridge**:
A service that connects one external messaging network to Matrix, landing each conversation in a portal room. Implementations (mautrix-whatsapp, mautrix-gmessages, …) are identified by `bridge_id` and are never network values.
_Avoid_: connector

**Portal room**:
An end-to-end encrypted Matrix room, maintained by a bridge, that holds exactly one external conversation.

**Sensor**:
The Matrix client that decrypts portal-room events, enriches them with contact and channel context, and publishes them as typed CloudEvents.

**Bus**:
The durable event stream that stores every Twalk event, with replay, filtering, and at-least-once delivery.
_Avoid_: queue, broker

### Agents

**Hermes**:
The agent platform: hosts personas, consumes events from the bus, reasons with an LLM, and publishes suggestions and replies back on the bus.

**Persona**:
A named agentic identity hosted by Hermes (e.g. `assistant`, `watch`, `archive`, `writing`) that observes events and produces suggestions or replies under human oversight. Contract event types live in the `persona.*` domain.
_Avoid_: bot, agent (as a product term; "agent" stays acceptable for the generic concept)

### Contract

**Contract**:
The versioned CloudEvents 1.0 envelope shared by every component, with type names of the form `fr.linagora.twalk.<domain>.<action>.<version>`. The schemas are the source of truth for every component.
_Avoid_: API, spec

**Network**:
An external messaging service a conversation comes from, as the user experiences it: WhatsApp, Signal, Telegram, Discord, SMS. A network outlives its transports: SMS is `sms` whether it transits through mautrix-gmessages or the SMS Companion.
_Avoid_: channel (user-facing copy only), gmessages (a bridge, not a network)

**Consent**:
The data-processing agreement state of a contact or channel: `granted`, `pending`, or `revoked`. Personas must not process events whose consent is not `granted`. The Companion Gateway is the single writer of consent state; Messagr and Buzz only render it.

### Companion

**Companion**:
The self-hosted PWA that guides a non-technical user from account bootstrap to bridge login, persona activation, and consent management. "The Companion" always means the PWA alone.
_Avoid_: using "Companion" for the Companion Gateway or the SMS Companion

**Companion Gateway**:
The backend serving the Companion: bridge provisioning facade, persona orchestrator, and single writer of consent state. Always named in full.

**SMS Companion**:
The first-party Android app (brand name: Twake SMS Companion) that reads and sends SMS on the user's phone and forwards them to the Twalk server over an end-to-end encrypted Matrix session; becomes the reference SMS path in v0.2.

### Ecosystem

**Messagr**:
LINAGORA's personal messaging interface; renders the user's conversations. Built outside this repo.

**Buzz**:
LINAGORA's agent-oversight interface, organised around Control Room, Watch Room, and Dialogue Room patterns. Built outside this repo.

**Twake**:
The LINAGORA product family Twalk belongs to; gives Twalk its name (Twake + Talk).
