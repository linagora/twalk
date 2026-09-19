# Twalk

Twalk is a sovereign, self-hosted event hub that turns fragmented personal messaging (WhatsApp, Signal, SMS, Telegram, Discord) into a single auditable stream of typed CloudEvents that agents observe, reason about, and respond to under human control.

## Language

### Collection and normalisation

**Bridge**:
A service that connects one external messaging network to Matrix, landing each conversation in a portal room. Implementations (mautrix-whatsapp, mautrix-gmessages, …) are identified by `bridge_id` and are never network values.
_Avoid_: connector

**Bridge bot**:
A bridge's **own** Matrix account — mautrix's `sender_localpart`, `@whatsappbot`, `@signalbot` — as distinct from the *ghosts* it materialises for people. It creates portal rooms, puppets ghosts, invites the user, and is a member of every portal room of its network. It is neither the owner nor a contact and has no consent state, so nothing is published about it on any event type and no decision about it enters the consent model (ADR 0026). Which accounts they are is handed to the Sensor by the deployment, never inferred: every inference available can suppress a real person instead, and an account that was not named stays a contact.
_Avoid_: "bot" unqualified (a persona is never a bot), or "bridge user" when the ghost is what is meant

**Portal room**:
An end-to-end encrypted Matrix room, maintained by a bridge, that holds exactly one external conversation. Built **lazily**, when that conversation becomes active, and not once at login: the set grows all day, which is why observing it is a continuous mechanism and not a step (ADR 0024).

**Portal register**:
Which portal rooms this deployment's bridges have built, and where the Sensor stands in each: `observing`, `invited` or `absent`. Read live from the homeserver **as each bridge's own bot** — the account the operator names in `GATEWAY_BRIDGE_<ID>_BOT_USER_ID`, never the appservice's `sender_localpart`, which is a generated account in no rooms (#171) — and kept nowhere, so the answer is the Sensor's own membership rather than a record that could drift from it. Its purpose is as much to state a number as to change one — "the Sensor is outside 17 of your 18 conversations" is a sentence the deployment can say (ADR 0024). Every bridge is in the answer whether it could be read or not, and a bridge that was read says which account asked and how many rooms that account is in, so a zero is never two facts at once. It is a mechanism and not a policy: the Sensor observes nothing by default, and which conversations it enters is the user's decision, one conversation at a time.

**Observation**:
The user's decision that Twalk may watch **one conversation** — the third unit Twalk decides in, beside consent (per contact) and persona activation (per network). It exists because the unit the model reasons in is not the unit the user thinks in: for a two-person conversation a contact and a conversation are the same thing, and for a 246-member association nobody adjudicates 246 people one by one. Expressed as the Sensor's membership of that conversation's portal room and nothing else, so starting and stopping are one mechanism in two directions. It does not grant consent and never has: inside an observed conversation each sender still starts at `pending` and their own consent governs what is published about them (ADR 0012, ADR 0024).
_Avoid_: "monitoring"; and never "observation" for what a persona is allowed to read, which is consent

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
A reply a persona proposes and never sends: it exists to be read, edited or refused by a human. Written in the language of the message it answers, not the user's (ADR 0016). Read from the bus, never kept: the listing the Companion Gateway serves is a projection of the stream and holds no copy, so a replay and the list cannot disagree — and what it says about the message being answered is that message's identity, never its words, because an excerpt belongs to the author of the quoted message and not to whoever sent the event carrying it (ADR 0012). Contract type: `persona.suggest.produced`.

**Approval**:
The human act that turns a suggestion into an outbound reply, carrying the identity of whoever approved it. Deliberate by construction — an explicit call, never a default, never a batch — and refused if the sender's consent is no longer `granted` at that moment. Served and published by the Companion Gateway, because that last clause is a read of the consent state and the Gateway is its single writer (ADR 0022); the event's `source` names the persona whose suggestion was approved, not the publisher. Contract type: `persona.reply.approved`.

### Contract

**Contract**:
The versioned CloudEvents 1.0 envelope shared by every component, with type names of the form `fr.linagora.twalk.<domain>.<action>.<version>`. The schemas are the source of truth for every component.
_Avoid_: API, spec

**Network**:
A messaging service a conversation comes from, as the user experiences it: WhatsApp, Signal, Telegram, Discord, SMS, or Matrix itself for native rooms (the bring-your-own-account channel, ADR 0009). A network outlives its transports: SMS is `sms` whether it transits through mautrix-gmessages or the SMS Companion. On every event but one it is the network of the room the traffic arrived in; on `inbound.presence.updated` it is the network **the subject is on**, because presence is not room-scoped and a room's answer would be somebody else's (ADR 0027).
_Avoid_: channel (user-facing copy only), gmessages (a bridge, not a network)

**Consent**:
**The user's** decision about whether a contact's messages, or a whole network's, may be processed: `granted`, `pending`, or `revoked`. A network-level decision is the default for that network; a per-contact decision always overrides it. Personas must not process events whose consent is not `granted`. The Companion Gateway is the single writer of consent state; Messagr and Buzz only render it.

The name is a term of art and it is **not the contact's own consent**: the contact is neither asked nor told, and `granted` records that the user decided, not that anyone agreed. Writing "the contact's agreement" here for months is how the gap went unnoticed, so the distinction stays in the glossary rather than in a comment somewhere. Whether and how a contact should be informed is open (issue #122); a persona's own disclosure to the contact is a separate mechanism (ADR 0019).

**Consent snapshot**:
The consent state of every known subject at one point in the event stream, served by the Companion Gateway and read by a consumer whose cache is cold (ADR 0010). It names the stream position it reflects, and states revocations explicitly: an absent subject means no decision was ever recorded, never a revoked one.

**Event families**:
The contract's two groups of event types. Message-flow events (`inbound.*`, `outbound.*`, `persona.*`) always carry the `network` extension, and carry `consent` whenever the event is about a contact; operational events (`consent.state.changed`, `bridge.status.changed`) declare both optional. The message-flow events that are not about a contact are the `outbound.*` family — the user's own message and their own reaction — and they carry no `consent` extension at all: the schema refuses one, because the extension is a contact's decision and the user is not a contact (ADR 0018, ADR 0021).

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
The single human a deployment serves, named as a Matrix ID in the Companion Gateway's configuration (`GATEWAY_OWNER`). Any number of devices, exactly one owner: multi-user deployments are out of scope (ADR 0011). The owner is never a contact and never has a consent state, on any event: their own messages and reactions are published as `outbound.message.sent` and `outbound.reaction.added` with their Matrix ID as the subject and no `consent` extension, and their own presence is not published at all (ADR 0018, ADR 0021).
_Avoid_: "admin", or "the user's account" when the owner's identity is what is meant

**Owner identity**:
A Matrix ID the owner's own traffic — messages, reactions, presence — is observed to arrive under. Normally a *network ghost* the bridge materialised for the owner's own account (`@whatsapp_33612345678`, `@whatsapp_lid-115332874281144`, `@signal_<uuid>`) — several per network, indistinguishable in shape from a contact's ghost, and not derivable from a bridge login id. The set is confirmed by the deployment and handed to the Sensor; an identity that is not confirmed stays a contact, because unknown is not the owner.
_Avoid_: calling one "the owner's Matrix ID", which is the account and never a ghost

**Device token**:
The Companion Gateway's own per-device credential, issued once a Matrix OpenID token has proved the owner's identity, carried as an `HttpOnly` cookie on the Gateway's origin, and revocable per device. Distinct from a Matrix access token, which the Gateway never holds (ADR 0011).
_Avoid_: calling it an access token, or a session

**Model configuration**:
The OpenAI-compatible endpoint, the model name that endpoint knows, an optional credential and a free-form object of provider parameters passed through untouched: what a persona reasons with. There is no default and Twalk ships no model, so a persona refuses to start without one (ADR 0015). Held by the Companion Gateway, set from the Companion, injected by the Hermes runtime into each persona's environment — never fetched by a persona, because the credential that reads it also opens the consent snapshot. The recommended shape is an OpenAI-compatible proxy in front of the model, which leaves the provider parameters as an escape hatch rather than the norm. A credential the operator supplied as a file **wins** over one set from the browser.
_Avoid_: "the LLM settings" when the language preference is also meant

**Native language**:
The user's own language, one of the five the Companion ships. It governs the interface, the explanations, and the language a persona falls back to when it cannot tell what language the message it is answering was written in (ADR 0016). It never governs the text sent to a contact: a suggestion follows the conversation. Stored rather than read from the browser, because a persona runs in a container and cannot read `navigator.language`. Unset is a state of its own and is not English.
_Avoid_: "locale", "the user's language" for the language a suggestion is written in

**SMS Companion**:
The first-party Android app (brand name: Twake SMS Companion) that reads and sends SMS on the user's phone and forwards them to the Twalk server over an end-to-end encrypted Matrix session; becomes the reference SMS path in v0.2.

### Ecosystem

**Messagr**:
LINAGORA's personal messaging interface; renders the user's conversations. Built outside this repo.

**Buzz**:
LINAGORA's agent-oversight interface, organised around Control Room, Watch Room, and Dialogue Room patterns. Built outside this repo.

**Twake**:
The LINAGORA product family Twalk belongs to; gives Twalk its name (Twake + Talk).
