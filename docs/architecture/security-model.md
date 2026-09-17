# Security model — which component holds which secret, and what is not protected

Twalk's promise is that no message leaves the user's infrastructure without their consent. This document states what that promise rests on: which component holds which secret, what is encrypted and what is not, where the trust boundaries run, and what consent does and does not do. It is written for an operator deciding whether to run Twalk on their own hardware, so it states residual risk plainly rather than reassuring.

Vocabulary is [`CONTEXT.md`](../../CONTEXT.md)'s. Two markers run through the document, because much of the design is decided and not yet built:

- **Today** — true of the code in this repository: the Sensor (`sensor/`) and the reference deployment (`deploy/docker-compose/`).
- **Designed, not built** — settled by an ADR or a spec issue, with no code behind it. The Companion Gateway, the Companion PWA and the bridges are all in this state: `companion-gateway/`, `companion/` and `bridges/` hold a README each. The Companion Gateway **does not exist yet**; every statement about what it holds is a statement about what its spec commits it to hold ([#46](https://github.com/linagora/twalk/issues/46), [#47](https://github.com/linagora/twalk/issues/47)).

Last reviewed: 2026-09-17.

---

## Trust boundaries

Nine boundaries matter. The important ones are not the network hops but the two places where encryption ends: inside a bridge, and inside the Sensor.

| Boundary | What crosses it | What protects it |
| --- | --- | --- |
| Network ↔ bridge | The user's traffic on WhatsApp, Signal, SMS, and the credentials that reach it | The network's own protocol. The bridge terminates it: this is where sovereignty starts, not where it holds |
| Bridge ↔ homeserver | Portal-room events, as an appservice | The bridge's `as_token` and the homeserver's `hs_token`, both in the appservice registration file |
| Homeserver ↔ Sensor | Megolm-encrypted portal-room events | End-to-end encryption. Synapse stores ciphertext it cannot read; it holds all the metadata |
| Sensor ↔ bus | Cleartext CloudEvents: bodies, attachment decryption keys, contacts | Nothing cryptographic. This is where end-to-end encryption ends, by design: normalisation needs plaintext |
| Bus ↔ consumers | Every event, replayable for the retention period | Network reachability only. In the reference stack the bus is bound to localhost and has no authentication |
| Browser ↔ Companion Gateway | The user's decisions, a Matrix OpenID token, network credentials in transit | TLS from the operator's reverse proxy, a per-device Gateway token, same origin ([ADR 0011](adr/0011-gateway-authenticates-with-matrix-openid.md)) |
| Companion Gateway ↔ homeserver and bridges | Account registration, bridge logins, status webhooks | Operator-supplied secrets in the Gateway's configuration — the powerful ones (see below) |
| Hermes runtime ↔ personas | Events, suggestions, replies | Nothing beyond the bus. Personas are separate processes with no privilege gap from third-party ones ([ADR 0008](adr/0008-hermes-rust-runtime-personas-as-processes.md)) |
| Persona ↔ LLM | Whatever the persona sends to reason about an event | The operator's choice of model. A remote model is outside the user's infrastructure |

Two consequences are worth stating before the detail:

- **The Sensor is the chokepoint of confidentiality, not of encryption.** Everything downstream of it — the bus, Hermes, personas, oversight interfaces — reads message bodies in cleartext. What limits them is the consent label and their willingness to honour it, not cryptography.
- **A bridge sees everything on its network.** WhatsApp and Google still see exactly what they saw before Twalk existed, and the bridge sees it too, in cleartext, with the credentials to act as the user. Self-hosting moves that exposure onto the user's own machine; it does not remove it.

---

## Which component holds which secret

| Secret | Held by | Why it is there | What it grants | Status |
| --- | --- | --- | --- | --- |
| Sensor account password (`SENSOR_PASSWORD`) | `.env` on the host, the Sensor's environment | The first login, and the interactive-authentication step that bootstraps cross-signing | Full control of the Sensor's Matrix account | Today |
| Sensor session access token (`session.json`, mode 0600) | The Sensor's state volume | So a restart restores its device instead of minting a new one | Acting as the Sensor's device | Today |
| Megolm room keys, cross-signing private keys, key-backup key | The Sensor's crypto store (sqlite) on the same volume | Decryption is the Sensor's job | Reading every portal room the Sensor is in, and its backed-up history | Today |
| `SENSOR_RECOVERY_KEY` (optional) | `.env`, the Sensor's environment | Recovering the **Sensor account's** cryptographic identity and key backup on a replacement device | The Sensor account's backed-up room keys | Today |
| Synapse signing key, macaroon secret, form secret | The `synapse-data` volume and `.env` | Server identity and session macaroons | Forging the homeserver's identity and its access tokens | Today |
| Registration shared secret | `.env`, rendered into `homeserver.yaml` | Provisioning accounts while open registration stays off | Creating accounts on the homeserver | Today (the provision one-shot); designed for the Gateway's registration relay |
| Sensor service token | Gateway configuration and Sensor configuration | Authenticating the Sensor's read of the consent snapshot ([ADR 0010](adr/0010-consent-snapshot-then-deltas.md)) | Reading the whole consent snapshot: every known contact | Designed, not built |
| Each bridge's `as_token` | The bridge's appservice registration, and the Gateway's configuration for verifying that bridge's status webhook | The bridge authenticates to the homeserver with it; the Gateway checks incoming webhooks against it | **Impersonating the appservice**: acting as any user in that bridge's namespace | Designed, not built |
| Each bridge's `hs_token` | The same registration file, used by the homeserver | The homeserver authenticates itself to the bridge | Feeding a bridge forged homeserver traffic | Designed, not built |
| Each bridge's provisioning secret | Gateway configuration | Driving logins and logouts through the bridge's provisioning API | Starting or destroying that bridge's login | Designed, not built |
| The Gateway's device-token signing key | Gateway configuration or store | Issuing and revoking per-device tokens ([ADR 0011](adr/0011-gateway-authenticates-with-matrix-openid.md)) | Minting a device token: the whole Gateway API, including writing consent | Designed, not built |
| Network credentials (WhatsApp session, Signal identity, the SMS path's Google cookies) | Inside each bridge's own store | Only the bridge can use them | Full access to the user's account on that network | Designed, not built |
| LLM credentials | The persona process | A persona reasons with a local or remote model | Whatever the model account allows; a remote model also sees what the persona sends it | Designed, not built |
| The user's Matrix password and recovery key | The user's browser, their password manager, their paper | Nothing server-side needs them | Everything the user's own account can do | Designed, not built (screen 2) |

What the Companion Gateway deliberately does **not** hold, per [ADR 0011](adr/0011-gateway-authenticates-with-matrix-openid.md): no Matrix access token for the user, no network credentials (a bridge login's cookies or QR payload pass through and are forgotten), no recovery key, and no message content — its pending-contact projection stores a contact's Matrix ID, its network and timestamps, never a body and never a display name.

The asymmetry is deliberate and uncomfortable: the Gateway holds no user credential and no message, but it does hold the two credentials that are most powerful on the homeserver — a registration secret that creates accounts, and an `as_token` that impersonates an appservice. ADR 0011 accepted that cost explicitly, and answers it with two narrow mitigations: the Gateway refuses any account creation after the first, and it uses the `as_token` only to verify a webhook's sender, never to act as the appservice. Neither mitigation changes what the credential would grant an attacker who read it.

---

## What is encrypted at rest, and what is not

**Encrypted, because Matrix encrypted it before it was stored.** Portal-room traffic sits in Synapse's database as Megolm ciphertext, and attachments sit in the media store as AES-CTR ciphertext with per-file keys. Synapse holds no key for either. That is the only at-rest encryption in the stack, and it covers message payloads alone — not the room graph, not memberships, not timestamps, not who talked to whom.

**Not encrypted: the Sensor's stores.** The Sensor opens its sqlite state and crypto stores with no passphrase (`sqlite_store(state_dir, None)` in [`sensor/src/main.rs`](../../sensor/src/main.rs)). The crypto store holds the Megolm keys, the cross-signing private keys and the key-backup key; the state store holds the room list and the sync token; `session.json` beside them holds the access token, written atomically with mode 0600. Anyone who can read that volume can read every room the Sensor can read.

This was decided, not overlooked. Issue [#14](https://github.com/linagora/twalk/issues/14) weighed a passphrase mechanism (an environment variable or a key file) against accepting the exposure, and accepted it: the host volume is the operator's to protect, with filesystem permissions and disk encryption. The reasoning behind that choice is worth keeping visible, because it is a judgment about a threat model rather than a claim of safety. The Sensor must open its store unattended on every restart, so any passphrase it could use has to live on the same host as the store — in the environment or in a file next to it. That defends against a disk that leaves the host, which is what full-disk encryption already defends against, while adding a new way for a restart to fail. The decision records one condition for revisiting it: if a hosted offering ever appears, the operator and the user stop being the same person and the trade-off changes.

**Not encrypted: everything else on disk.** Synapse's sqlite database, its signing key and its media store; the bus's JetStream file store; and `.env`, which holds every secret above in cleartext and is readable by anyone who can read the file or inspect the containers. No volume in the reference deployment is encrypted by the stack itself. Disk encryption, volume permissions and host hardening are the operator's, and the documentation says so rather than implying the containers handle it.

**The bus is the widest exposure, and it is not at rest only.** Events carry message bodies, contact identities and — since the contract gained encryption material on attachments ([#17](https://github.com/linagora/twalk/issues/17), closing the gap found in [#15](https://github.com/linagora/twalk/issues/15)) — the AES key of every encrypted attachment, in cleartext, for as long as the stream's retention keeps them. The schema itself says the key must stay inside the user's infrastructure and must never be logged ([`inbound.message.received.schema.json`](../../contracts/cloudevents/v1/inbound.message.received.schema.json)). In the reference stack the bus has no authentication at all and is bound to localhost for that reason ([`compose.yaml`](../../deploy/docker-compose/compose.yaml)). A key alone does not fetch the file — Synapse's media API still wants an account — but a key plus any account on the homeserver does.

---

## The recovery-key boundary

Two different recovery keys exist in Twalk, and conflating them is the mistake this section is here to prevent.

**The user's recovery key belongs to the browser.** During account creation (screen 2 of [`docs/wireframes/companion-v0.1.md`](../wireframes/companion-v0.1.md)) the Companion generates the key client-side, shows it once, and offers a copy button and a PDF. [ADR 0011](adr/0011-gateway-authenticates-with-matrix-openid.md) makes this binding: the key is generated in the browser and never reaches the Gateway. The Companion is a static export with no secret of its own, and the Gateway relays registration without ever seeing the key that protects the account's secret storage.

Screen 2's copy — "Twalk never sees it" — is therefore a statement about where the code runs, and it holds on one condition, which is now a requirement on the PWA and not a matter of taste: the key must be generated by the browser, used in the browser to encrypt the cross-signing secrets, and only the ciphertext and the key's own description may be uploaded to the homeserver. Matrix's secret storage is built for exactly this, so meeting the condition costs nothing — but an implementation that posted the key to the Gateway "to make recovery easier" would break a promise the user was shown in plain language.

**The Sensor has its own key, because it is a different account.** The Sensor logs in as `@sensor:<domain>`, a Matrix account of its own with its own device, its own cross-signing identity and its own server-side key backup, created automatically when none exists. `SENSOR_RECOVERY_KEY` is that account's recovery key, saved by the operator during onboarding and read from `.env`. It is not the human's, and it could not be: the human's key opens the human's secret storage, which has nothing the Sensor needs, and putting the human's key in a server-side environment variable is precisely what ADR 0011 and screen 2 rule out.

The trade-off is symmetric and small in both directions. Set, `SENSOR_RECOVERY_KEY` lives in `.env` and in the container's environment, and its blast radius is the Sensor account's backed-up room keys — the traffic of the rooms the Sensor was invited to, nothing on the user's own devices. Unset, the Sensor decrypts live traffic anyway, because senders share Megolm keys with its device directly, but a replacement device loses everything from before it existed. The Sensor says so loudly in the log when it has to start on a clean crypto store.

---

## Consent as a confidentiality guarantee

Consent is not a preference the interfaces render. It decides what leaves the homeserver onto the bus, and what a persona may read once it is there. The Companion Gateway is its single writer ([ADR 0006](adr/0006-consent-state-owned-by-companion-gateway.md)); consumers read a snapshot naming the stream position it reflects and follow the stream from there ([ADR 0010](adr/0010-consent-snapshot-then-deltas.md)); activating or pausing a persona is itself a consent decision, scoped to the networks that persona may read ([ADR 0013](adr/0013-persona-activation-is-a-consent-decision.md)).

The three states protect different things, and only one of them is enforced by the producer:

- **`granted`.** The full event is published. Everything downstream may read it.
- **`pending`.** The Sensor labels, consumers refuse ([ADR 0012](adr/0012-revoked-consent-reduces-publication.md)). The body **is** on the bus. For a pending contact the guarantee is a convention honoured by consumers, not a property of the event — worth knowing before trusting a third-party persona.
- **`revoked`.** The Sensor still publishes an inbound event, but carrying no content ([ADR 0012](adr/0012-revoked-consent-reduces-publication.md)). The event keeps its deterministic id, network, consent label, both timestamps, room and sender references, reply and thread relations, the contact's display name, and each attachment's shape — kind, media type, size, dimensions, duration. It drops everything that is the message: the body (which, for a media message, is also where a filename would travel), the reply excerpt and the reaction target's excerpt, and inside each attachment the `mxc://` reference, its decryption material and its caption. A reference plus the key that opens it is the content, so the two go together. Publishing nothing at all was rejected: the user would lose the evidence that a message arrived, and a broken bridge would become indistinguishable from a silent contact. Unlike `pending`, this state is enforced by the contract, not only honoured by the producer: `inbound.message.received.v1` makes such an event *invalid* if it carries a body, an excerpt or a media reference under a `revoked` label ([#58](https://github.com/linagora/twalk/issues/58)).

**What `revoked` stops:** future publication of that contact's message content — bodies, excerpts and media references alike — from the moment the Gateway records the decision and the Sensor applies it.

**What `revoked` does not erase,** stated as a list because every item has surprised someone:

- events already on the bus, which stay until retention expires and remain replayable in full;
- the portal rooms and the media store on the homeserver, which are untouched;
- the bridge's own store, and the external network's servers, which never heard about the decision;
- the Gateway's pending-contact projection, which keeps the contact's identity and timestamps;
- anything a persona already read, cached, or sent to an LLM.

Revocation applies to the future only. Erasing a contact's history is a separate, explicit action, scheduled for v1.0 alongside audit export and import ([`roadmap.md`](roadmap.md)). ADR 0012 rejected conflating the two in both directions: it surprises the user who only wanted to stop, and deceives the one who believed they had erased.

Two more properties follow from the design. A paused persona is not de-provisioned: it keeps running and receives nothing, and it keeps whatever it already had. And activation never spreads — connecting a new network leaves every persona inactive on it until the user says otherwise, which is the one behaviour the project exists to prevent.

Today the consent path has a failure mode, and it fails closed: the Sensor's consent cache is in memory and no snapshot source exists yet, so after a restart it labels every sender `pending` ([#16](https://github.com/linagora/twalk/issues/16), fixed by the Gateway's snapshot endpoint). Over-labelling as `pending` is the safe direction, but it means that until the Gateway lands, a persona that honours the contract legitimately processes nothing.

---

## Residual risks

Plainly, with no mitigation implied where none exists.

1. **No volume in the reference deployment is encrypted** (today). Read access to the Sensor's volume yields the Megolm keys and the session token; read access to the bus's volume yields the retained events, bodies and attachment keys included. This is the accepted consequence of [#14](https://github.com/linagora/twalk/issues/14).
2. **The Companion Gateway concentrates the homeserver's most powerful credentials** (designed, not built). It can create an account and it holds a credential that impersonates an appservice. Compromising it reaches further than compromising the Companion, the bus, or any single bridge. Its own mitigations — refusing accounts after the first, using the `as_token` only to verify webhooks — constrain what the Gateway does, not what its secrets would allow.
3. **Network credentials live inside the bridges** (designed, not built). The Gateway relays them and forgets them, so the Gateway is not where they are stolen: each bridge is, and each is a full account takeover surface for one network. Bridges are upstream software Twalk does not fork, so their storage and their exposure are theirs.
4. **Message metadata accumulates in the Gateway's contact projection** (designed, not built). No bodies and no display names, but who contacted the user, on which network, and when — a social graph with timestamps, in a service reachable from the browser. Note also that a bridged Matrix ID conventionally embeds the network identifier (`@whatsapp_33612345678:example.com`), so storing the Matrix ID keeps the phone number even though the projection stores no identifier field of its own.
5. **The bus has no authentication** (today). Subject filtering is not a permission. Anything that can reach the bus port reads every event and can replay the stream.
6. **`pending` is enforced by consumers, not by the Sensor** (today). A persona that ignores the label reads bodies it was never granted.
7. **Every secret sits in cleartext in `.env` and in container environments** (today), including the Sensor's account password and, when set, its recovery key.
8. **One owner, no isolation between humans** (designed, not built). A deployment serves one person on any number of devices; there is no multi-user separation to lean on, and the Gateway's refusal to create a second account is also the weak point in any recovery story that loses the owner's identity.
9. **A remote LLM moves message content outside the infrastructure** (designed, not built). Consent gates whether a persona reads an event; it does not gate where that persona reasons. An operator who configures a hosted model has consented to that, and should know they did.
10. **The Sensor's invitation allow-list is the only control on which rooms it joins** (today). `SENSOR_ALLOWED_INVITERS` names the accounts whose invitations are honoured, and a bridge's provisioning user is on that list — so a compromised bridge can put the Sensor into a room of its choosing.

---

## What would change this model

- A hosted offering: the operator and the user stop being the same person, and [#14](https://github.com/linagora/twalk/issues/14)'s decision has to be reopened along with everything else in this document that says "the operator's to protect".
- Erasure (v1.0): the first operation that reaches backwards through the bus, the homeserver and the projections at once.
- Authentication on the bus, and per-consumer authorisation: the change that would turn the `pending` convention into an enforced boundary.
- A second login per bridge instance, or a second human: both need a dimension the contract does not have yet.

Vulnerabilities are reported privately, following the process the [README](../../README.md#security) points at.
