# Twalk

Twalk is a sovereign, self-hosted event hub that perceives **what happens between the user and other people, across the accounts they hold** — messaging first (WhatsApp, Signal, SMS, Telegram, Discord, Matrix), then email, calendars and contacts — and turns it into a single auditable stream of typed CloudEvents that agents observe, reason about, and respond to under human control. That frontier is the point: an event naming nobody but the user needs none of these rules, so a build or a feed belongs straight to an agent and Twalk would add only a translation layer with nothing behind it (ADR 0033).

## Language

### Collection and normalisation

**Bridge**:
A service that connects one external messaging network to Matrix, landing each conversation in a portal room. Implementations (mautrix-whatsapp, mautrix-gmessages, …) are identified by `bridge_id` and are never network values.
_Avoid_: connector

**Bridge bot**:
A bridge's **own** Matrix account — mautrix's `sender_localpart`, `@whatsappbot`, `@signalbot` — as distinct from the *ghosts* it materialises for people. It creates portal rooms, puppets ghosts, invites the user, and is a member of every portal room of its network. It is neither the owner nor a contact and has no consent state, so nothing is published about it on any event type and no decision about it enters the consent model (ADR 0026). Which accounts they are is handed to the Sensor by the deployment, never inferred: every inference available can suppress a real person instead, and an account that was not named stays a contact.
_Avoid_: "bot" unqualified (a persona is never a bot), or "bridge user" when the ghost is what is meant

**Login flow**:
The sequence of steps a bridge asks the user to complete before it will act as their account on a network. Its **shape is the bridge's** — how many steps there are, what each asks for, and in what order, which the bridge does not itself know in advance: a two-factor password step appears only for an account that has one, so a step is named and never counted. Its **words are the project's** wherever honesty needs more than a bridge says (ADR 0030). A refused answer ends the flow rather than re-asking, because the bridge drops the process with the refusal.
_Avoid_: "authentication" for this, which is the Companion's own sign-in; "pairing", which is one kind of step

**Login step**:
One question in a login flow, with the fields it wants and the words explaining them. A field's **type** is what decides how it is drawn, so a type the deployment does not know is refused and named rather than guessed at — and fields the bridge groups by type, such as a jar of cookies, are collected through one control rather than one each. A step's answers pass through the Companion Gateway and are stored nowhere, logged nowhere, and written to no browser store (ADR 0011, ADR 0030).
_Avoid_: "form", which implies the Companion decided what it contains

**Portal room**:
An end-to-end encrypted Matrix room a bridge maintains for one external conversation — though a room can carry several conversations inside it as threads, which the bus already distinguishes and the register cannot, so an observation decision is taken at the grain of the **room** and covers every thread in it, present and future (ADR 0029). A room is where a conversation currently lives and not its identity: a network can replace the room while the conversation continues, and `m.room.tombstone` names the successor. Built **lazily**, when that conversation becomes active, and not once at login: the set grows all day, which is why observing it is a continuous mechanism and not a step (ADR 0024).

**Conversation**:
One exchange on a network, as the user experiences it: a person they write to, a group, a community's announcement channel. It is **not** a Matrix room, though a room is where it currently lives — a network can replace the room (a Telegram group promoted to a supergroup, a Matrix room upgraded) while the conversation continues, and the user's decision about it follows. Identified by the network's own id where the bridge states one and by the room id where the room genuinely *is* the conversation, as it is for a native Matrix room, which has no bridge and therefore no other name (ADR 0029).
_Avoid_: using "room" and "conversation" interchangeably; "chat"

**Handover room**:
The single encrypted Matrix room the owner's account and the Sensor share, created by the Companion in the browser during onboarding and holding nothing else. It exists for one measured reason: a browser can hand the Sensor an Olm-encrypted to-device message (ADR 0034) only if its crypto machine tracks the Sensor, and a user becomes tracked only through membership of a shared **encrypted** room — of which the owner's account had none, being merely invited to portal rooms it never joined. It is **not a conversation and cannot become one**: its `m.room.create` carries a type, which is the immutable marker every client already uses to keep a space out of a room list and which keeps this room out of the conversation chooser; no bridge ever hears of it, so the portal register never reaches it; and it is created with `events_default` above any power level a member can hold, so the homeserver refuses every message event from everybody including the owner. That last one is why nothing is ever published on the bus from it — not a rule the Sensor follows, but an event that cannot exist. Found again, when onboarding is re-run, by a canonical alias rather than by anything Twalk wrote down (#226).
_Avoid_: calling it a conversation, a chat or a control room; "the Sensor's room", which suggests the Sensor owns it

**Portal register**:
Which portal rooms this deployment's bridges have built, and where the Sensor stands in each: `observing`, `invited` or `absent`. Read live from the homeserver **as each bridge's own bot** — the account the operator names in `GATEWAY_BRIDGE_<ID>_BOT_USER_ID`, never the appservice's `sender_localpart`, which is a generated account in no rooms (#171) — and kept nowhere, so the answer is the Sensor's own membership rather than a record that could drift from it. Its purpose is as much to state a number as to change one — "the Sensor is outside 17 of your 18 conversations" is a sentence the deployment can say (ADR 0024). Every bridge is in the answer whether it could be read or not, and a bridge that was read says which account asked and how many rooms that account is in, so a zero is never two facts at once. It is a mechanism and not a policy: the Sensor observes nothing by default, and which conversations it enters is the user's decision, one room at a time. A conversation whose room was replaced appears once, at its successor, and carries the fact that it moved; the decision follows it unless the successor's audience crosses the threshold the user acknowledged, which returns it to the chooser (ADR 0029).

**Observation**:
The user's decision that Twalk may watch **one conversation** — the third unit Twalk decides in, beside consent (per contact and connection) and persona activation (per connection). It exists because the unit the model reasons in is not the unit the user thinks in: for a two-person conversation a contact and a conversation are the same thing, and for a 246-member association nobody adjudicates 246 people one by one. Expressed as the Sensor's membership of that conversation's portal room and nothing else, so starting and stopping are one mechanism in two directions. It does not grant consent and never has: inside an observed conversation each sender still starts at `pending` and their own consent governs what is published about them (ADR 0012, ADR 0024).
_Avoid_: "monitoring"; and never "observation" for what a persona is allowed to read, which is consent

**Sensor**:
The Matrix client that decrypts portal-room events, enriches them with contact and channel context, and publishes them as typed CloudEvents. It holds **two** Matrix identities and never confuses them: its own `@sensor:` account, which observes, and the *owner device*, which acts.

**Owner device**:
A device of the **user's own Matrix account**, created during onboarding and held by the Sensor, through which Twalk *acts* as the user rather than observing them (ADR 0025, ADR 0034). It exists because a bridge relays to its network only what the logged-in user's own account sends, so a reply from `@sensor:` is ignored without a log line. It is **write-only**: it joins the portal rooms of the bridges the deployment named — and only those, since everything in an invitation except its sender is chosen by whoever sent it — and posts approved replies; it reads no history, so it needs neither cross-signing nor a recovery key, and the user's recovery key is something no Twalk component ever asks for. It appears in the user's device list and revoking it from any Matrix client stops Twalk replying. It is never what the portal register reads as `observing`: observation is the Sensor's own membership, and ADR 0024's mechanism is untouched by it.
_Avoid_: "puppet" or "double puppeting", which is a bridge acting as the user and is the alternative ADR 0025 rejected; "the Sensor's device", which is the other identity

**Reach**:
What a posted reply actually got to, as distinct from where it was published: the **contact**, or **nobody**. A reply posted by the owner device into a portal room it has joined reaches the contact, because the bridge relays it; one posted by `@sensor:` into a portal room reaches nobody, and used to be reported as sent (#216). Said per reply on the bus and counted, because "published on your bus", "posted into a room" and "delivered to your contact" are three facts and only the last is what a user means by *sent*.
_Avoid_: "delivered" for a message that merely reached Matrix

**Bus**:
The durable event stream that stores every Twalk event, with replay, filtering, and at-least-once delivery.
_Avoid_: queue, broker

### Agents

**Persona runtime**:
The component that hosts personas, consumes events from the bus, and publishes suggestions and replies back on it. It is the **enforcement boundary**: the consent gate and the trigger allowlist live in the SDK it starts personas with, so what it may do is arranged rather than trusted — which is why it is not scaffolding to be removed when a more capable agent arrives (ADR 0032). It holds no model of its own: an operator-chosen endpoint is injected into each persona (ADR 0015). Called `hermes` in the source tree, and **every ADR before 0032 calls it Hermes** — those are records of decisions and are not rewritten, so a reader of ADR 0008, 0013, 0015, 0017, 0022 or 0023 should read *Hermes* there as this entry. Renaming the directory, the crate and its environment variables is a wide mechanical refactor of its own.
_Avoid_: **Hermes**, which now means only the agent below

**Hermes**:
Nous Research's agent runtime, external to this project and to this deployment: it reasons, keeps memories, and has skills that can act. Reached by a signed webhook from a persona, so that what it sees has already passed the consent gate; its answer returns through the Companion Gateway and carries the language it was written in (ADR 0032). It is not governed by this project — it ships messaging adapters of its own, and events that arrive through them carry none of Twalk's guarantees.

**Persona**:
A named agentic identity hosted by the persona runtime (e.g. `assistant`, `watch`, `archive`, `writing`) that observes events and produces suggestions or replies under human oversight. Contract event types live in the `persona.*` domain.
_Avoid_: bot, agent (as a product term; "agent" stays acceptable for the generic concept)

**Persona activation**:
The user's decision that a persona may read a given network, recorded as a consent decision on that persona and nothing else (ADR 0013). A paused persona is one whose consent was revoked: it still runs, and receives no events. Activation never spreads to a newly connected network on its own.

**Suggestion**:
A reply a persona proposes and never sends: it exists to be read, edited or refused by a human. Written in the language of the message it answers, not the user's (ADR 0016). Read from the bus, never kept: the listing the Companion Gateway serves is a projection of the stream and holds no copy, so a replay and the list cannot disagree — and what it says about the message being answered is that message's identity, never its words, because an excerpt belongs to the author of the quoted message and not to whoever sent the event carrying it (ADR 0012). Contract type: `persona.suggest.produced`.

**Disclosure**:
The line a persona-drafted reply carries to the contact receiving it, saying the message was drafted with the user's AI assistant. On by default; removable only for every conversation at once, never for one message, and the removal recorded as a dated act rather than kept as a preference (ADR 0019, ADR 0031). Written in the language of the reply and not the user's (ADR 0016), which is why the persona chooses it and neither the model nor the Gateway composes it. It states how *this* message was produced, so a message the user wrote themselves carries none — attaching it to everything would make it mean nothing. Distinct from whether a contact is told the conversation is observed at all, which is open (#122).
_Avoid_: "watermark", "notice"; and the word is also used in this repository for warnings shown to the **user** about a network's own risks, which is a different thing

**Approval**:
The human act that turns a suggestion into an outbound reply, carrying the identity of whoever approved it. Deliberate by construction — an explicit call, never a default, never a batch — and refused if the sender's consent is no longer `granted` at that moment. Refused too, since #275, when the connection the reply would leave by last said it cannot send (`connection_not_connected`): a reply published for a collector that will not take it is a reply into the void. Served and published by the Companion Gateway, because that last clause is a read of the consent state and the Gateway is its single writer (ADR 0022); the event's `source` names the persona whose suggestion was approved, not the publisher. Contract type: `persona.reply.approved`.

### Contract

**Contract**:
The versioned CloudEvents 1.0 envelope shared by every component, with type names of the form `fr.linagora.twalk.<domain>.<action>.<version>`. The schemas are the source of truth for every component.
_Avoid_: API, spec

**Network**:
The **kind** of a connection, as the user experiences it — what an icon and a card say. It is not what a consent decision is scoped to; that is the **connection** (ADR 0033). A messaging service a conversation comes from: WhatsApp, Signal, Telegram, Discord, SMS, or Matrix itself for native rooms (the bring-your-own-account channel, ADR 0009). A network outlives its transports: SMS is `sms` whether it transits through mautrix-gmessages or the SMS Companion. On every event but one it is the network of the room the traffic arrived in; on `inbound.presence.updated` it is the network **the subject is on**, because presence is not room-scoped and a room's answer would be somebody else's (ADR 0027).
_Avoid_: channel (user-facing copy only), gmessages (a bridge, not a network)

**Connection**:
One configured account, mailbox or calendar — *this* WhatsApp login, *this* work inbox. It is the **perimeter** a consent decision is scoped to, because a person may reach the user through two of them and a decision about one is not a decision about the other. A network is its kind, not its identity: collapsing the two would grant an employer's workspace and a personal one in a single gesture (ADR 0033).
_Avoid_: using "network" when an instance is meant

**Perimeter**:
What a consent decision is scoped to — always a connection. It is half of what consent *means*: the state is read by subject **and** perimeter, so an event attributed to the wrong one makes the model read the wrong row (ADR 0027).

**Collector**:
A component that turns one source into contract events. The Sensor is the Matrix collector — the role it always had, with one sense. A collector observes by **subscription and not by filtering**: it receives only what it subscribed to rather than receiving everything and discarding, which is as close to a structural property as a source without Matrix membership allows (ADR 0033).
_Avoid_: calling one a bridge, which connects a network to Matrix rather than a source to the bus

**Consent**:
**The user's** decision about whether a contact's messages, or a whole network's, may be processed: `granted`, `pending`, or `revoked`. A decision is scoped to a **connection** and not to a network, since a person may reach the user through two of them (ADR 0033). A connection-level decision is the default there; a per-contact decision always overrides it. A person reached through two sources is **two subjects the user may declare linked**, never merged by inference — ADR 0018's rule about the owner's own identities, applied to a third party, where a wrong merge grants somebody else's consent. Personas must not process events whose consent is not `granted`. The Companion Gateway is the single writer of consent state; Messagr and Buzz only render it.

The name is a term of art and it is **not the contact's own consent**: the contact is neither asked nor told, and `granted` records that the user decided, not that anyone agreed. Writing "the contact's agreement" here for months is how the gap went unnoticed, so the distinction stays in the glossary rather than in a comment somewhere. Whether and how a contact should be informed is open (issue #122); a persona's own disclosure to the contact is a separate mechanism (ADR 0019).

**Consent snapshot**:
The consent state of every known subject at one point in the event stream, served by the Companion Gateway and read by a consumer whose cache is cold (ADR 0010). It names the stream position it reflects, and states revocations explicitly: an absent subject means no decision was ever recorded, never a revoked one.

**Words**:
The free text a **contact** wrote, or a persona wrote about it: a message's body, a quoted excerpt wherever one appears, a suggestion and its rationale, an attachment's caption, and an attachment's decryption material. Published as **its own event on its own stream**, kept days rather than months, because portal rooms are end-to-end encrypted and the bus is therefore the only place in a deployment where other people's plaintext exists at rest (ADR 0028). The line is *whose words*, not content against metadata: the owner's own writing, and what was sent under their identity, stay with the identity. A consumer that must not read a contact's words is one that does not subscribe — explicit rather than enforced, since the reference bus has no authentication.
_Avoid_: "content" or "payload" for the words of a specific person; "the message" when only its identity is meant

**Event families**:
The contract's three groups of event types. Message-flow events (`inbound.*`, `outbound.*`, `persona.*`) always carry the `network` extension, and carry `consent` whenever the event is about a contact; operational events (`consent.state.changed`, `bridge.status.changed`, `connection.status.changed`) declare both optional, the last one carrying `connection` since its subject is the connection itself; and the calendar events (`calendar.event.*`, ADR 0033) carry `connection` and neither `network` — a calendar is a kind of connection, not a network — nor `consent`, because their subject is the owner (their own agenda) and the people inside them are governed by the participant rule the contract states in words, on the mail connection of the same account. A message-flow event carries the **identity** of what happened; the **words** travel separately and expire sooner (ADR 0028), so an event whose words are gone is a complete event and not a damaged one. The message-flow events that are not about a contact are the `outbound.*` family — the user's own message and their own reaction — and they carry no `consent` extension at all: the schema refuses one, because the extension is a contact's decision and the user is not a contact (ADR 0018, ADR 0021).

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
A Matrix ID the owner's own traffic — messages, reactions, presence — is observed to arrive under. Normally a *network ghost* the bridge materialised for the owner's own account (`@whatsapp_33612345678`, `@whatsapp_lid-115332874281144`, `@signal_<uuid>`) — several per network, indistinguishable in shape from a contact's ghost, and not derivable from a bridge login id. The set is confirmed by the deployment and handed to the two components that need it — the Sensor, which publishes that traffic as the owner's own, and the Companion Gateway, which refuses a consent decision about one of them and serves none of them in its consent state or its snapshot (#149). One list, read twice: it grows after the fact, so two components maintaining their own copies would drift. An identity that is not confirmed stays a contact, because unknown is not the owner.
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
Block's open-source, self-hostable workspace on the Nostr protocol, where humans and agents share channels and every message is a signed event on a relay the operator owns. Built outside this repo and not by LINAGORA, which this entry claimed until ADR 0032. It is where the user talks to Hermes and where approvals surface — a **surface** for approval and never its authority, since the Companion Gateway remains the single writer (ADR 0022).

**Clerk**:
The Twalk component that writes onto Buzz what the bus says and carries the owner's ✅ to the Companion Gateway **as a device of theirs** (ADR 0036): signed in like a phone, named `Buzz` on the dashboard, revoked there like one. A surface and never an authority (ADR 0032): it holds no consent snapshot, no store, no key of Hermes's, and no credential the Gateway would take for anyone but the owner's own device — one refresh token, never the service token. Called *le greffier* in French.
_Avoid_: "Buzz bot", "the Twalk agent", "herald"

**Twake**:
The LINAGORA product family Twalk belongs to; gives Twalk its name (Twake + Talk).
