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

**Suggestion**:
A reply a persona proposes and never sends: it exists to be read, edited or refused by a human. Written in the language of the message it answers, not the user's (ADR 0016). Contract type: `persona.suggest.produced`.

**Approval**:
The human act that turns a suggestion into an outbound reply, carrying the identity of whoever approved it. Deliberate by construction — an explicit call, never a default, never a batch — and refused if the sender's consent is no longer `granted` at that moment. Contract type: `persona.reply.approved`.

### Contract

**Contract**:
The versioned CloudEvents 1.0 envelope shared by every component, with type names of the form `fr.linagora.twalk.<domain>.<action>.<version>`. The schemas are the source of truth for every component.
_Avoid_: API, spec

**Network**:
A messaging service a conversation comes from, as the user experiences it: WhatsApp, Signal, Telegram, Discord, SMS, or Matrix itself for native rooms (the bring-your-own-account channel, ADR 0009). A network outlives its transports: SMS is `sms` whether it transits through mautrix-gmessages or the SMS Companion.
_Avoid_: channel (user-facing copy only), gmessages (a bridge, not a network)

**Consent**:
**The user's** decision about whether a contact's messages, or a whole network's, may be processed: `granted`, `pending`, or `revoked`. A network-level decision is the default for that network; a per-contact decision always overrides it. Personas must not process events whose consent is not `granted`. The Companion Gateway is the single writer of consent state; Messagr and Buzz only render it.

The name is a term of art and it is **not the contact's own consent**: the contact is neither asked nor told, and `granted` records that the user decided, not that anyone agreed. Writing "the contact's agreement" here for months is how the gap went unnoticed, so the distinction stays in the glossary rather than in a comment somewhere. Whether and how a contact should be informed is open (issue #122); a persona's own disclosure to the contact is a separate mechanism (ADR 0019).

**Consent snapshot**:
The consent state of every known subject at one point in the event stream, served by the Companion Gateway and read by a consumer whose cache is cold (ADR 0010). It names the stream position it reflects, and states revocations explicitly: an absent subject means no decision was ever recorded, never a revoked one.

**Event families**:
The contract's two groups of event types. Message-flow events (`inbound.*`, `outbound.*`, `persona.*`) always carry the `network` extension, and carry `consent` whenever the event is about a contact; operational events (`consent.state.changed`, `bridge.status.changed`) declare both optional. The one message-flow event that is not about a contact is `outbound.message.sent`, the user's own message, and it carries no `consent` extension at all — the schema refuses one, because the extension is a contact's decision and the user is not a contact (ADR 0018).

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

**Owner**:
The single human a deployment serves, named as a Matrix ID in the Companion Gateway's configuration (`GATEWAY_OWNER`). Any number of devices, exactly one owner: multi-user deployments are out of scope (ADR 0011). The owner is never a contact and never has a consent state: their own messages are published as `outbound.message.sent` with their Matrix ID as the subject (ADR 0018).
_Avoid_: "admin", or "the user's account" when the owner's identity is what is meant

**Owner identity**:
A Matrix ID the owner's own messages are observed to arrive under. Normally a *network ghost* the bridge materialised for the owner's own account (`@whatsapp_33612345678`, `@whatsapp_lid-115332874281144`, `@signal_<uuid>`) — several per network, indistinguishable in shape from a contact's ghost, and not derivable from a bridge login id. The set is confirmed by the deployment and handed to the Sensor; an identity that is not confirmed stays a contact, because unknown is not the owner.
_Avoid_: calling one "the owner's Matrix ID", which is the account and never a ghost

**Device token**:
The Companion Gateway's own per-device credential, issued once a Matrix OpenID token has proved the owner's identity, carried as an `HttpOnly` cookie on the Gateway's origin, and revocable per device. Distinct from a Matrix access token, which the Gateway never holds (ADR 0011).
_Avoid_: calling it an access token, or a session

**SMS Companion**:
The first-party Android app (brand name: Twake SMS Companion) that reads and sends SMS on the user's phone and forwards them to the Twalk server over an end-to-end encrypted Matrix session; becomes the reference SMS path in v0.2.

### Ecosystem

**Messagr**:
LINAGORA's personal messaging interface; renders the user's conversations. Built outside this repo.

**Buzz**:
LINAGORA's agent-oversight interface, organised around Control Room, Watch Room, and Dialogue Room patterns. Built outside this repo.

**Twake**:
The LINAGORA product family Twalk belongs to; gives Twalk its name (Twake + Talk).
