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

**Persona activation**:
The user's decision that a persona may read a given network, recorded as a consent decision on that persona and nothing else (ADR 0013). A paused persona is one whose consent was revoked: it still runs, and receives no events. Activation never spreads to a newly connected network on its own.

### Contract

**Contract**:
The versioned CloudEvents 1.0 envelope shared by every component, with type names of the form `fr.linagora.twalk.<domain>.<action>.<version>`. The schemas are the source of truth for every component.
_Avoid_: API, spec

**Network**:
A messaging service a conversation comes from, as the user experiences it: WhatsApp, Signal, Telegram, Discord, SMS, or Matrix itself for native rooms (the bring-your-own-account channel, ADR 0009). A network outlives its transports: SMS is `sms` whether it transits through mautrix-gmessages or the SMS Companion.
_Avoid_: channel (user-facing copy only), gmessages (a bridge, not a network)

**Consent**:
The data-processing agreement state of a contact or a whole network: `granted`, `pending`, or `revoked`. A network-level decision is the default for that network; a per-contact decision always overrides it. Personas must not process events whose consent is not `granted`. The Companion Gateway is the single writer of consent state; Messagr and Buzz only render it.

**Consent snapshot**:
The consent state of every known subject at one point in the event stream, served by the Companion Gateway and read by a consumer whose cache is cold (ADR 0010). It names the stream position it reflects, and states revocations explicitly: an absent subject means no decision was ever recorded, never a revoked one.

**Event families**:
The contract's two groups of event types. Message-flow events (`inbound.*`, `persona.*`) always carry the `network` and `consent` extensions; operational events (`consent.state.changed`, `bridge.status.changed`) declare them optional.

### Companion

**Companion**:
The self-hosted PWA that guides a non-technical user from account bootstrap to bridge login, persona activation, and consent management. "The Companion" always means the PWA alone.
_Avoid_: using "Companion" for the Companion Gateway or the SMS Companion

**Companion Gateway**:
The backend serving the Companion: bridge provisioning facade, persona orchestrator, and single writer of consent state. Always named in full.

**Onboarding**:
The guided path the Companion walks a new user through: homeserver, account, recovery key, one or more networks, a first persona, then the dashboard. It is the user's journey, not a state machine the software stores — progress is read from what actually exists (an account, a connected bridge, an active persona).

**Recovery key**:
The 48-character secret that unlocks the user's own encrypted history, generated in their browser and shown once (ADR 0014). Twalk never holds it. The Sensor's own key backup uses a separate key of its own, which the operator configures: the two are never the same secret.

**SMS Companion**:
The first-party Android app (brand name: Twake SMS Companion) that reads and sends SMS on the user's phone and forwards them to the Twalk server over an end-to-end encrypted Matrix session; becomes the reference SMS path in v0.2.

### Ecosystem

**Messagr**:
LINAGORA's personal messaging interface; renders the user's conversations. Built outside this repo.

**Buzz**:
LINAGORA's agent-oversight interface, organised around Control Room, Watch Room, and Dialogue Room patterns. Built outside this repo.

**Twake**:
The LINAGORA product family Twalk belongs to; gives Twalk its name (Twake + Talk).
