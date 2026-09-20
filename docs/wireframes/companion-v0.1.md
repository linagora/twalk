# Twalk Companion — Wireframes v0.1

This document describes the key screens of the Companion PWA for the v0.1 milestone (end of 2026). It was written as design input for the frontend implementation, and validated before development started.

v0.1 ships onboarding for **four networks**: **WhatsApp**, **Signal**, **SMS via mautrix-gmessages (proof of concept)**, and **an existing Matrix account** (bring your own account, without a fresh registration). Each network has its own dedicated screen because the login mechanism, the failure modes and the trust story differ substantially between them.

**On the SMS path in v0.1 vs v0.2.** v0.1 uses [mautrix-gmessages](https://docs.mau.fi/bridges/go/gmessages/authentication.html) as a proof-of-concept SMS bridge. This lets us prove the end-to-end pipeline (SMS observed, published on the bus, surfaced to the assistant persona, replied to and sent back) without committing to a mobile release cadence in v0.1. The trade-off is transparent to the user: SMS transits through Google Messages Web infrastructure and requires a Google account. v0.2 replaces this path with the sovereign first-party Twake SMS Companion Android application published by LINAGORA on F-Droid. Both flows are described below (screen 3c for v0.1, screen 3c-next for v0.2).

Wireframes are described textually rather than visually to keep the document versionable in the monorepo, reviewable in pull requests, and translatable to any design tool without ambiguity. Each screen documents its purpose, layout, interactive elements, states, and success criteria.

Design tokens (colors, typography, spacing) follow the Messagr design system, itself aligned with the Twake product family. All screens are designed mobile-first (viewport 375 to 428 CSS pixels wide), with responsive extensions for tablet and desktop.

---

## This document is not the authority — the ADRs and the code are

These screens were validated before any of the Companion existed. Since then a design review (issue [#74](https://github.com/linagora/twalk/issues/74)), twenty-one ADRs and the shipped screens have overruled parts of them. A reader who takes an uncorrected wireframe for the current design learns things that are not true, so this document is kept in step with `docs/architecture/adr/` (why the structural choices are what they are), `CONTEXT.md` (which word to use) and `companion/src/routes/` (what the product actually does). Where the three disagree, the wireframe is the one that is wrong.

Two conventions carry that, and both exist so the document can be read beside the code:

- **Overruled.** blocks say what the wireframe showed, what replaced it, and which decision replaced it. Nothing is silently deleted: a screen that quietly disappears teaches nothing, and the reader who wonders why the obvious design was not taken is the reader the ADRs exist for.
- **Shipped.** blocks name the route the screen lives at and the differences the implementation found — several of them are facts about the world (a bridge that renders no QR image, a browser that cannot read another origin's cookies) that no amount of design review would have surfaced.

### The decisions that landed since this document was validated

| Decision | What it changes in these screens |
|---|---|
| [ADR 0009](../architecture/adr/0009-matrix-is-a-network.md) | Native Matrix traffic is a network of its own (`network=matrix`), not the absence of one. Screen 3d's rooms reach the bus instead of being dropped. |
| [ADR 0010](../architecture/adr/0010-consent-snapshot-then-deltas.md) | Consent is read from the Companion Gateway's snapshot and then followed on the bus. The Gateway's journal is the record of truth; the bus is the audit trail. |
| [ADR 0011](../architecture/adr/0011-gateway-authenticates-with-matrix-openid.md) | Each device signs itself in with a Matrix OpenID token and gets its own revocable device token. There is no device-to-device pairing — screen 1's "pair with my other device" cannot exist. |
| [ADR 0012](../architecture/adr/0012-revoked-consent-reduces-publication.md) | A revoked contact's event keeps its identity and loses its body, and a suggestion carries the answered message's **identity** and never its words. That is why the approval screen cannot name the contact. |
| [ADR 0013](../architecture/adr/0013-persona-activation-is-a-consent-decision.md) | Activating a persona is a consent decision on that persona, scoped to networks, and it never spreads to a network connected later. Screen 4 offers nothing that could not be written as that one decision. |
| [ADR 0014](../architecture/adr/0014-companion-crypto-runs-in-the-browser.md) | The recovery key is generated in the browser and shown once, and losing the crypto store is a designed journey (`/recover`) rather than an error. |
| [ADR 0015](../architecture/adr/0015-no-default-llm-configured-through-the-companion.md) | There is no default LLM: a persona refuses to start without a model configuration. No screen in this document configures one — see the gap noted under screen 4. |
| [ADR 0016](../architecture/adr/0016-a-reply-follows-the-conversation-not-the-user.md) | A suggestion is written in the language of the message it answers, not the user's. The user's native language is a stored preference, and the interface ships in five languages, not two. |
| [ADR 0018](../architecture/adr/0018-the-users-own-messages-are-their-own-event-type.md), [ADR 0021](../architecture/adr/0021-the-owner-is-never-a-contact-on-any-event.md) | The owner is never a contact: their own messages are `outbound.message.sent`, their reactions `outbound.reaction.added`, and their presence is not published at all. |
| [ADR 0019](../architecture/adr/0019-a-persona-discloses-itself-to-the-contact.md) | Every reply a persona drafted discloses itself to the contact, in a sentence this project chooses, on by default and removable only on the record. Built by [#121](https://github.com/linagora/twalk/issues/121): the switch and its record are a card on the settings screen, and the approval screen shows the sentence fixed beside the editable reply (ADR 0031). |
| [ADR 0020](../architecture/adr/0020-observing-a-foreign-homeserver-is-a-device-of-the-users-account.md), [ADR 0025](../architecture/adr/0025-twalk-acts-as-the-user-through-a-device-of-their-account.md) | The bring-your-own-account path is two-staged: v0.1 watches only an account on the homeserver this deployment drives, and a foreign homeserver is later watched by **a device of the user's own account**. The same device is what posts an approved reply as the user. |
| [ADR 0022](../architecture/adr/0022-the-approval-api-lives-on-the-companion-gateway.md) | Approval is served and published by the Companion Gateway, one suggestion at a time, and refused if the sender's consent is no longer `granted` at that moment. This document had no approval screen at all; there is one now. |
| [ADR 0024](../architecture/adr/0024-a-portal-room-is-observed-by-invitation-per-conversation.md) | A portal room is built **lazily**, as a conversation becomes active, and the Sensor observes nothing by default: which conversations it enters is the user's decision, one at a time. No screen here asked that question. |

### Where each screen lives

| Screen | Route | Tickets |
|---|---|---|
| 1 — Welcome and homeserver | `/` | [#66](https://github.com/linagora/twalk/issues/66), [#67](https://github.com/linagora/twalk/issues/67) |
| 2 — Account and recovery key | `/onboarding` | [#67](https://github.com/linagora/twalk/issues/67) |
| 3 — Network picker | `/networks` | [#68](https://github.com/linagora/twalk/issues/68), [#120](https://github.com/linagora/twalk/issues/120) |
| 3a / 3b / 3c — WhatsApp, Signal, SMS | `/networks/whatsapp`, `/networks/signal`, `/networks/sms` (each with a `/manage` screen, [#108](https://github.com/linagora/twalk/issues/108)) | [#68](https://github.com/linagora/twalk/issues/68), [#55](https://github.com/linagora/twalk/issues/55) |
| 3d — Existing Matrix account | `/networks/matrix` | [#68](https://github.com/linagora/twalk/issues/68), [#124](https://github.com/linagora/twalk/issues/124), [#138](https://github.com/linagora/twalk/issues/138) |
| 4 — Persona activation | `/personas` | [#69](https://github.com/linagora/twalk/issues/69) |
| 5 — Home dashboard | `/dashboard` | [#69](https://github.com/linagora/twalk/issues/69), [#100](https://github.com/linagora/twalk/issues/100) |
| 6 — Approving a suggestion (added below) | `/approvals` | [#100](https://github.com/linagora/twalk/issues/100) |
| 7 — Choosing the observed conversations (added below) | `/networks/conversations` | mechanism [#105](https://github.com/linagora/twalk/issues/105), screen [#143](https://github.com/linagora/twalk/issues/143) |

Three screens exist that this document never designed, and they are not omissions to be corrected but paths the implementation discovered: `/signin`, where a device whose token expired signs back in ([#111](https://github.com/linagora/twalk/issues/111)); `/recover`, the store-loss journey ADR 0014 makes a normal event; and `/diagnostics`, which is what the Companion has instead of telemetry — a page the user can read and copy, transmitted nowhere.

And four things designed here do not exist. The **bottom navigation bar** (Home, Bridges, Personas, Consent, Settings) was never built: the Companion is a journey with a dashboard at its centre, not a tabbed app. The **consent inbox** is v0.2, so screen 5's chip states a number and says where the decisions are *not*. **Messagr pairing** is a card that says pairing is not available in this version. And every **Server-Sent Events** stream in this document is imaginary — see the notes on screens 3a and 5.

---

## Network matrix (bridging strategy for v0.1)

| Network | Bridge | Login mechanism | Notable constraints | Milestone |
|---|---|---|---|---|
| WhatsApp | mautrix-whatsapp | QR code, unofficial WhatsApp Web multi-device protocol via [whatsmeow](https://github.com/tulir/whatsmeow) | Personal (free) account works; account ban risk exists and must be disclosed to the user. See network screen 3a. | v0.1 |
| Signal | mautrix-signal | QR code, secondary device pairing via libsignal | Primary device (phone) must stay online. Group history is not backfilled. | v0.1 |
| SMS (PoC) | mautrix-gmessages | Google account cookie login (QR is [deprecated by Google](https://docs.mau.fi/bridges/go/gmessages/authentication.html)); requires Google Messages app active on the primary Android phone | Not sovereign: SMS transits through Google Messages Web. Requires a Google account. Not available for iOS-only users. Explicitly framed as a proof-of-concept path; migrated to the sovereign Twake SMS Companion in v0.2. | v0.1 |
| Matrix (existing) | Direct login on the user's homeserver | Password or SSO. QR sign-in ([MSC4108](https://github.com/matrix-org/matrix-spec-proposals/pull/4108)) is named on the screen and not offered: it carries the user's encryption secrets across and needs machinery these screens deliberately do not load | No bridge needed. **v0.1 watches only an account on the homeserver this deployment drives**, and the screen refuses a foreign one before asking for a credential (ADR 0020, [#138](https://github.com/linagora/twalk/issues/138)). The Sensor is invited into the rooms the user ticks | v0.1 |
| SMS (sovereign) | Twake SMS Companion (Android app by LINAGORA) using Android [SmsManager](https://developer.android.com/reference/android/telephony/SmsManager) and [Telephony.Sms](https://developer.android.com/reference/android/provider/Telephony.Sms) content provider, publishing to a dedicated Matrix appservice room | Requires an Android phone owned by the user; the phone must stay online for SMS to flow. No Google account or Google Play Services dependency. Not available for iOS-only users. Replaces the mautrix-gmessages v0.1 path. | **v0.2** |
| Telegram | mautrix-telegram | Phone number + code, or Telegram bot token | Standard Telegram authentication. | **v0.2** |
| Discord | mautrix-discord | OAuth login via Discord | Standard Discord OAuth. | **v0.2** |

**Shipped.** The reference deployment ships two bridges, `mautrix-whatsapp` and `mautrix-signal` (`bridges/`, `deploy/docker-compose/`). Screen 3c exists in the Companion and no gmessages bridge is configured for it, so on the reference deployment the SMS card is greyed with "No bridge for this network is configured on your Twalk server" — which is the picker doing its job, not a defect. A network this table lists and the deployment has no bridge for is always *shown* rather than hidden: hiding it leaves the user wondering where WhatsApp went.

---

## Screen 1 — Bootstrap: Welcome and homeserver

**Purpose.** First screen the user sees when opening the Companion for the first time. Establishes trust, states what Twalk does, and asks for the homeserver URL.

**Trigger.** A browser with no session on this Gateway. What the Companion knows about itself is asked of the Gateway (`GET /api/session`), never read from a progress flag it wrote into browser storage: onboarding progress is derived from what exists — an account, a connected bridge, an active persona — and this screen is the first place that rule shows.

**Layout (top to bottom).**

- Header: Twalk logo (32 px), title "Twalk Companion", subtitle "The command center for your sovereign messaging hub".
- Body: three-line description in the local language: "Twalk lets you bring all your conversations (WhatsApp, Telegram, Signal, Discord, Google Messages) into one place you own, watched over by agents you configure. This Companion sets everything up for you."
- Input field: "Your Twalk domain", placeholder "example.com", inline validation (must be a valid hostname).
- Primary CTA button: "Continue", disabled until the input is valid.

**States.**

- *Empty:* CTA disabled, no error.
- *Valid input:* CTA enabled.
- *Invalid input:* red micro-copy under the field, "Please enter a valid domain like `example.com`".
- *Loading (after Continue):* CTA replaced by a spinner, input disabled, subtitle changes to "Checking your Twalk deployment...".
- *Homeserver unreachable:* input turns red, error banner at the top "We could not reach `<domain>`. Check the URL or your internet connection." Retry available.

**Overruled — the secondary link "I already have a Twalk account, pair with my other device".** It is gone, and not because it was unbuilt. There is no device-to-device pairing in Twalk: **each device signs itself in**, by obtaining an OpenID token from the user's homeserver and exchanging it at the Companion Gateway for a device token of its own, which the Gateway can revoke per device ([ADR 0011](../architecture/adr/0011-gateway-authenticates-with-matrix-openid.md)). A pairing code would be a second identity source and a second attack path to test, for a deployment that already has one identity source. What replaced the link is two things: this screen *recognises* a browser that already holds a session and offers the dashboard instead of an account form, and `/signin` signs a device back in when its token expires.

**Shipped.** The loading state is a real probe of two questions in that order (`$lib/onboarding/deployment.ts`): whether the Gateway at this origin accepts anybody at all — a deployment with no owner configured is its own answer, "this Twalk deployment has no owner configured, so it accepts nobody yet" — and then Matrix's own discovery on the domain. So there are five failures on this screen and not one: no answer from the Gateway, no owner configured, an unreachable domain, a domain that answered without being a homeserver, and an invalid domain. This screen is also where a returning user with a live session and a lost crypto store is sent to `/recover` before being asked to type a domain, which is what makes store loss a journey rather than an error ([ADR 0014](../architecture/adr/0014-companion-crypto-runs-in-the-browser.md)).

**Success criteria.** User understands what Twalk does in under 20 seconds, enters their domain, and reaches Screen 2.

---

## Screen 2 — Bootstrap: Create account and recovery key

**Purpose.** Provision the user's Matrix account on the homeserver, generate cross-signing keys, and get the user to save the recovery key.

**Trigger.** User completed Screen 1 with a reachable homeserver that supports self-service registration.

**Layout.**

- Header: back arrow, title "Create your account".
- Body top: field "Username", empty, with the Matrix address it will produce shown as it is typed. Validation: 3 to 30 characters, lowercase, digits, underscore, hyphen.
- Body middle: field "Password" and "Confirm password". Password strength meter. Minimum 12 characters, at least one letter and one digit.
- Body bottom: informational card explaining what a recovery key is: "Your recovery key unlocks your encrypted conversations if you lose this device. It is generated here, in this page, and Twalk never sees it. Write it down or save it in a password manager: you cannot recover it later."
- Primary CTA: "Create my account".

**Second step within the same screen (after account creation).**

- Header changes to "Save your recovery key".
- Body shows the recovery key in a large, readable font (mono, tabular numerals): **48 characters in 12 groups of four**, `AAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH JJJJ KKKK LLLL MMMM`. That is what Matrix's own base58 encoding of a 32-byte key produces (`$lib/recovery/key.ts`); the earlier "32 chars grouped by 4" in this document was wrong, and the `/recover` screen tells the user the same number when it asks for the key back.
- A line saying this is the only time the key is shown, and that nothing on the Twalk server and nothing at Twalk can show it again.
- Copy button next to the key.
- Download button ("Save as PDF") that generates a printable one-page PDF with the key and human instructions.
- Card: **what is lost without it.** "This key is what unlocks your encrypted conversations on a new device. Without it, and with this device gone, your message history stays encrypted for ever: nobody — not your Twalk server, not Twalk, not your administrator — can read it back or reset it. Your other devices can no longer be verified either." Plus, on its own line, "There is no reset in this version. A lost key cannot be replaced; only a new account can."
- Checkbox: "I have saved my recovery key in a safe place".
- Primary CTA: "Continue", disabled until the checkbox is ticked.
- A line on the key backup: whether it started, and if it did not, that the recovery key is still valid and diagnostics are the next step.

**States.**

- *Username taken:* red under the field, "This username is already taken on `<domain>`".
- *Not the owner this deployment expects:* "This deployment was set up for a different username. Ask your administrator which one it expects." One deployment serves one owner ([ADR 0011](../architecture/adr/0011-gateway-authenticates-with-matrix-openid.md)), so the username is not free.
- *The deployment already has its account:* "One Twalk deployment serves one person, and that account exists. If this is your deployment and your keys are gone from this device, take the recovery path" — with a way to `/recover`, because that is what this situation usually is.
- *Registration closed:* "This Twalk deployment does not allow new accounts. Its account was created by its administrator."
- *Bootstrap in progress:* a named step rather than one spinner — loading the encryption engine, creating the cryptographic identity, generating the recovery key, securing the key backup, signing this device in.

**Corrected — the recovery key copy.** The original card said only that the key cannot be recovered later. It now says **what is lost**, because v0.1 ships no reset flow: a user who reads "you cannot recover it later" and thinks of a password reset has not been told the truth. The wording above is the shipped copy, and `CONTRIBUTING.md` asks translators to keep it uncomfortable rather than soften it.

**Shipped.** The whole cryptographic bootstrap runs **in the browser** with matrix-js-sdk's Rust WebAssembly stack ([ADR 0014](../architecture/adr/0014-companion-crypto-runs-in-the-browser.md)), in an order that is the point of that ADR and lives in `$lib/crypto/bootstrap.ts`: cross-signing, then the recovery key, then secret storage with the key backup. The account itself is created through the Companion Gateway's registration relay, which refuses any account after the first. Three consequences reach this screen. The crypto store lives unencrypted in IndexedDB, so losing it is a normal event with its own journey (`/recover`) rather than an error. Two tabs sharing that store would corrupt it, so one tab holds a Web Lock and the others say so. And a browser that cannot do the job at all — iOS Lockdown Mode, or a page outside a secure context — is detected and explained *before* onboarding starts, never mid-flow. The screen also refuses to send a request carrying key material, and says so as a bug in the Companion if it ever happens.

**Success criteria.** User has an account, has saved their recovery key with intention (not just clicked through), and reaches Screen 3.

---

## Screen 3 — Network picker

**Purpose.** After account creation, the user chooses which networks to connect. This screen is the entry point to the four v0.1 network-specific flows (screens 3a, 3b, 3c, 3d). Users can connect one network now and skip the rest for later. Telegram and Discord are shown as "coming in v0.2" cards so the user knows the roadmap.

**Trigger.** User completed Screen 2, or opens the Companion from the dashboard via "Add a network".

**Layout.**

- Header: back arrow, title "Connect your first network", step indicator "1 of 3 to finish setup" — **only when that is true**, see the correction below.
- Body: card grid, one card per network.
  - **Active cards (v0.1):**
    - **WhatsApp** — icon, name, subtitle "Personal or business account, via WhatsApp Web". Tap opens screen 3a.
    - **Signal** — icon, name, subtitle "Secondary device via Signal linked devices". Tap opens screen 3b.
    - **SMS via Google Messages (preview)** — icon, name, subtitle "Proof of concept via Google Messages. Sovereign path arrives in v0.2". Tap opens screen 3c. A discreet badge "Preview" is shown on the card. Grayed out with a helper tooltip if the browser reports an iOS user agent.
    - **Existing Matrix account** — icon, name, subtitle "Connect a Matrix account you already own". Tap opens screen 3d.
  - **Coming soon cards (v0.2, non-interactive, muted styling):**
    - **Telegram** — subtitle "Coming in v0.2".
    - **Discord** — subtitle "Coming in v0.2".
- Body bottom: link "Skip for now, I'll add networks later" (grayed until at least the account is created).
- Footer: informational chip "You can add or remove networks at any time from the dashboard."

**States.**

- *No network connected yet:* skip link is subtle but not disabled.
- *At least one network connected:* connected networks are marked as connected and get a "Manage" secondary action, which opens the manage screen for that network ([#108](https://github.com/linagora/twalk/issues/108)).

**Corrected — the heading is derived, not fixed.** The wireframe's "Connect your first network — 1 of 3 to finish setup" was shown unconditionally to anyone who reached this screen, including a returning user adding a second network to a working deployment. Observed live with WhatsApp connected for two hours and Signal for two minutes, every word of it was false ([#120](https://github.com/linagora/twalk/issues/120)). There are **three** headings and not two, because "I could not ask" is not "nothing is connected": the onboarding wording when `GET /api/bridges` says nothing is connected, "Add a network" when something is, and a neutral heading when the Gateway did not answer — which claims nothing, where *first network* and *step 1 of 3* are claims that can be false. Progress is read from what exists on the Gateway, never from a flag the Companion wrote into browser storage.

**Shipped.** Whether a network is connected is the bridge's own answer and nobody else's: `GET /api/bridges` carries a `connection` read from the bridge's `whoami`, and this screen reads that and never `login`, which is a QR scan in flight — reading the wrong one is what made a live WhatsApp link show no badge after a Gateway restart ([#108](https://github.com/linagora/twalk/issues/108)). A bridge the Gateway could not reach is *unknown*, not disconnected. And the screen distinguishes a server that did not answer from one that refused: `GET /api/bridges` answering `401` in zero milliseconds is an expired session, and telling the owner their Twalk server could not be reached sends them to inspect a firewall for a problem that is a sign-in ([#111](https://github.com/linagora/twalk/issues/111)).

**Success criteria.** Users understand that Twalk supports several networks and that they can start with just one.

---

## Screen 3a — WhatsApp login (QR code)

**Purpose.** Complete WhatsApp bridge login. This is the anchor experience for v0.1: if this works smoothly, the user believes in Twalk. If it stumbles, the user abandons.

**Trigger.** User tapped the WhatsApp card on screen 3.

**Layout.**

- Header: back arrow, title "Connect WhatsApp".
- Body top: visual hierarchy — a stylized illustration of two devices (a smartphone showing WhatsApp, this device showing Twalk). Below, the caption "Twalk logs into WhatsApp as an additional WhatsApp Web session. Your conversations stay end-to-end encrypted between WhatsApp and Twalk; Twalk just observes them."
- **Disclosure card (mandatory before showing the QR):** informational card, amber tone, with a heading "About WhatsApp accounts": "Twalk connects to WhatsApp via the same Web protocol as WhatsApp Web. It works with your personal (free) account and does not require a WhatsApp Business account. WhatsApp does not officially support third-party clients: in rare cases, accounts using Twalk may be temporarily suspended or banned by WhatsApp. Twalk minimizes this risk by respecting the Web protocol as strictly as possible, but the risk is not zero. You can dismiss this notice once, and it will not appear again on this device." Two buttons: "I understand, continue" and "Learn more" (opens a documentation page).
- Body middle: the QR code, **drawn in the browser** from the raw payload `GET /api/bridges/{bridge_id}/login` hands over, and redrawn when that answer's `generation` changes.
- Body bottom: instructions in three numbered steps: "1. Open WhatsApp on your phone. 2. Go to Settings, then Linked Devices, then Link a Device. 3. Scan this QR code with your phone camera."
- Footer: "Troubleshooting" link that opens a bottom sheet with common issues, including "My phone must stay online at least once every 12 days for the link to remain active" and "WhatsApp allows up to 4 linked devices per account".

**States.**

- *Disclosure shown:* QR code hidden until the user confirms the disclosure. Confirmation is stored locally in the Companion PWA and does not expire.
- *Waiting for scan:* QR code visible, subtle countdown showing when it expires.
- *QR expired:* QR replaced by a "Refresh" button.
- *Scanned, waiting for confirmation:* QR replaced by a spinner and "Verifying your session...".
- *Success:* full-screen success animation (checkmark), "WhatsApp connected. Twalk is now observing your WhatsApp conversations." Continue button returns to screen 3.
- *Failure:* error banner with a specific reason (session revoked, phone offline, the login lost to a restart, the network refusing another linked device, a passkey the Companion cannot present). Retry button. If the failure is a suspected ban, an escalated red card is displayed with next steps.
- *A login already in progress on another device:* named with the device and the time it started, with "Take it over on this device" or "Leave it running" — only one login at a time is possible on a network.
- *The bridge list unreadable:* a terminal state with a way out, never a spinner. This is the incident this screen caused: the owner waited on "Asking for a code…" and reported "I cannot get a QR code", and the Signal bridge had never been contacted, because there was no bridge id to start a login with ([#111](https://github.com/linagora/twalk/issues/111)).

**Overruled — Server-Sent Events.** There is no event stream on the Companion Gateway, and no request is held open. The Gateway holds the bridge's blocking step *inside itself* and answers `GET /api/bridges/{bridge_id}/login` immediately with the current step, its payload and how long it is valid ([#55](https://github.com/linagora/twalk/issues/55)); the Companion **polls** and redraws on a new `generation`. That is why a phone that sleeps mid-scan loses a poll rather than the login — the failure mode an SSE connection through a browser's background tab would have had. The rest of the login API is `GET /api/bridges/{bridge_id}/login/flows`, `POST .../login`, `POST .../login/submit` and `GET/DELETE .../logins`.

**Shipped.** The countdown is drawn because the wireframe asks for it, and it is not the fact the screen keys off: `valid_for_seconds` is the Gateway's own estimate of the network's refresh interval (WhatsApp's whole budget is about 2m40 across refreshes), and the fact is the generation. The QR is drawn here because **the bridge renders no image** — it hands over a payload, and rendering it is the browser's job. On success the Companion Gateway does publish `bridge.status.changed.v1`, and it is the sole producer of that type, but not because this screen asked: each bridge pushes its connection state to `POST /_twalk/bridges/{bridge_id}/status`, authenticated by that bridge's own `as_token`, and the Gateway maps mautrix's vocabulary to the contract's and publishes the transition ([#56](https://github.com/linagora/twalk/issues/56)). Nothing on this origin serves those events back to a browser yet, which is why screen 5 says what it says.

**Success criteria.** 90% of first-time users complete this screen in under 3 minutes without opening the troubleshooting sheet, and are aware of the ban risk before they scan.

**Also shipped, and not designed here.** `/networks/whatsapp/manage` ([#108](https://github.com/linagora/twalk/issues/108)): which account is linked, since when, how it is doing, and the two actions that exist — Disconnect and Re-link. Screens 3b and 3c have the same.

---

## Screen 3b — Signal login (QR code, secondary device)

**Purpose.** Pair Twalk as a secondary device on the user's Signal account, using the standard Signal linked devices mechanism.

**Trigger.** User tapped the Signal card on screen 3.

**Layout.**

- Header: back arrow, title "Connect Signal".
- Body top: caption "Twalk becomes a secondary Signal device on your account, just like Signal Desktop. Your primary phone stays required for account recovery."
- Body middle: the QR code, from mautrix-signal's payload through the same `GET /api/bridges/{bridge_id}/login` as screen 3a and drawn in the browser the same way.
- Body bottom: instructions numbered for both Android and iOS: "1. Open Signal on your phone. 2. Go to Settings, then Linked Devices. 3. Tap the plus button (Android) or Link New Device (iOS). 4. Scan this QR code."
- Below instructions, an informational chip in a calm tone: "Signal officially supports secondary devices. There is no ban risk. History from before this pairing is not synchronized: Twalk will see new messages only."
- Footer: "Troubleshooting" link.

**States.** Same lifecycle as screen 3a (waiting for scan, expired, scanning, success, failure, a login already running on another device) but without the mandatory disclosure card because there is no ban risk to disclose.

**Shipped.** One component serves screens 3a, 3b and 3c (`$lib/components/login/BridgeLogin.svelte`), because the Gateway reports one shape for every bridge and the screens differ in their words. It owns the login's *flow* and delegates each kind of step to a panel of its own, through a table typed over the view's discriminant, so a step kind nobody draws is a compile error (ADR 0030) — which is what a `{#if}` chain could not give, and is why an `input`, `cookies` or `emoji` step used to draw an empty box here. So screen 3a's correction applies here unchanged: polling, not Server-Sent Events; the QR drawn in the browser; `bridge.status.changed.v1` published by the Gateway from the bridge's own status push.

**Success criteria.** User completes pairing in under 2 minutes and understands the history limitation.

---

## Screen 3c — SMS via Google Messages (v0.1 preview)

**Status.** Implemented in v0.1 as a proof of concept, and replaced by screen 3c-next in v0.2. The *screen* is shipped (`/networks/sms`); the reference deployment ships no gmessages bridge for it yet, so on that deployment the card is greyed and the screen says which bridge is missing.

**Purpose.** Complete the mautrix-gmessages bridge login. This is the v0.1 SMS path: it validates the end-to-end SMS pipeline (Sensor decryption, CloudEvents publication, agent processing, outbound reply) without committing to a mobile release. The sovereignty gap is disclosed to the user upfront.

**Trigger.** User tapped the SMS card on screen 3.

**Layout.**

- Header: back arrow, title "Connect SMS via Google Messages", small "Preview" badge.
- **Preview and migration disclosure (mandatory before showing the login flow):** informational card, amber tone: "This SMS path is a v0.1 proof of concept. It relies on Google Messages Web, which means SMS transit through Google's infrastructure and a Google account is required. In v0.2, Twalk replaces this path with a first-party Android app that reads SMS directly from your phone with no third-party in the middle. Your existing SMS setup will migrate automatically when v0.2 ships. Learn more." Two buttons: "I understand, continue" and "Learn more" (opens ADR 0004 rendered in a modal).
- Body top: informational card explaining the requirement: "Twalk uses the Google Messages app on your Android phone to send and receive SMS. You need Google Messages installed and set as your default messaging app on Android. iPhone-only users cannot use this preview."
- Body middle: three steps.
  - **Step 1: Enable Google Messages Web on your phone.** Instructions: "1. Open Google Messages on your Android phone. 2. Tap your profile picture, then Device pairing. 3. Leave that screen open: your phone has to be reachable for the rest of this."
  - **Step 2: Hand over your Google Messages session.** Because Google [deprecated the QR code login](https://docs.mau.fi/bridges/go/gmessages/authentication.html) for third-party bridges in 2024, mautrix-gmessages signs in with the user's Google session cookies. **The user copies them by hand** and pastes them here — see the correction below — and the screen states the two browser settings that otherwise make the copy useless: a private window (signing out of Google in a normal window invalidates the session the bridge is holding), and Device Bound Session Credentials off in Chrome (it ties the session to this device's hardware key, on purpose, so a copied cookie works nowhere else, and it is on by default in recent versions).
  - **Step 3: Confirm the pairing emoji on the phone.** Google Messages shows an emoji; the screen shows the same one and waits for the confirmation to happen on the phone.
- Body bottom: "Troubleshooting" link with common issues (Google 2FA challenges, session revoked by Google, phone offline).

**States.**

- *Disclosure shown:* login flow hidden until the user confirms the disclosure. Confirmation is stored locally in the Companion PWA.
- *iOS user detected:* screen replaced by a friendly explanation, "SMS through Twalk in v0.1 requires an Android phone. If you have both an iPhone and an Android device, you can use the Android device as your SMS source. The v0.2 sovereign SMS path will still require an Android phone as well." With a "Skip SMS for now" button.
- *Cookies unreadable or incomplete:* which cookies are missing, by name, and the two accepted formats (`name=value; name=value`, or the JSON a cookie extension exports).
- *Waiting for the phone:* the pairing emoji, and "Waiting for you to confirm on your phone…".
- *Connected:* "SMS is connected through Google Messages", plus "This network migrates to the sovereign path on its own when v0.2 ships. Nothing to do now."
- *Failure:* specific error messages for a Google two-factor challenge left unanswered, cookies copied from a window that has since signed out, Device Bound Session Credentials still on, or a session already active on too many devices.

**Overruled — the in-app browser, and "stored encrypted".** Two things in the original step 2 cannot be built as described, and one of them was a promise.

The in-app browser is impossible, not merely unimplemented: a PWA has none, and no page may read another origin's cookies — that is the same-origin policy, and it is the rule protecting the user's other accounts rather than a gap to work around. So the user copies the cookies themselves, and the screen's job becomes saying plainly what they are handing over: "together, these are your Google sign-in session: anything holding them can act as you on Google Messages Web for as long as they last". They go to `POST /api/bridges/{bridge_id}/login/submit`, are relayed to the bridge and forgotten by the Gateway ([ADR 0011](../architecture/adr/0011-gateway-authenticates-with-matrix-openid.md), asserted by reading the Gateway's whole state directory and its logs for the credential just relayed), and this screen writes them **nowhere** — not `localStorage`, not IndexedDB; the textarea is cleared the moment it is submitted.

"This cookie … is stored encrypted" was false and is removed. Network credentials — the WhatsApp session, the Signal identity, these Google cookies — sit **unencrypted** in each bridge's own store, in the deployment's volumes, which [`security-model.md`](../architecture/security-model.md) states in its credential table and accepts as a residual risk ([#14](https://github.com/linagora/twalk/issues/14)). A privacy screen that promises encryption the stack does not perform is worse than one that says nothing. What the shipped copy says instead is where they go, that Twalk promises no lifetime for them because Google documents none, and that the user can end the session from their Google account at any time.

**Overruled — Server-Sent Events.** As on screen 3a: the Gateway answers a pollable login state, and there is no stream. `bridge.status.changed.v1` with `network=sms` and a `bridge_id` naming the mautrix-gmessages instance is still what a successful login produces, published by the Gateway from the bridge's own status push ([#56](https://github.com/linagora/twalk/issues/56)).

**Shipped — on the same renderer as 3a and 3b.** Since [ADR 0030](../architecture/adr/0030-a-logins-shape-is-the-bridges-and-its-words-are-ours.md) this screen is `$lib/components/login/BridgeLogin.svelte` like the QR ones, and the cookie step is drawn by the same field-type-driven panel every step goes through: a jar of `cookie` fields sharing a domain is one paste and one parser, because the bridge grouping them by type is a signal from the data rather than a special case for SMS. What stayed bespoke is the *prose* — step 1, step 2, the private window, Device Bound Session Credentials, and what the cookies let their holder do — which is keyed on the bridge's own step id and **replaces** its eleven words. That keying can rot, so the screen declares the shape the explanation was written against and says so on screen, falling back to the bridge's words, if the step ever stops matching.

**Migration to v0.2.** When v0.2 ships, users on the mautrix-gmessages path see a persistent banner on their dashboard offering a one-click migration to the Twake SMS Companion. During migration, both bridges run in parallel for 30 days to avoid message loss, then mautrix-gmessages is decommissioned per user. Migration mechanics are documented in the v0.2 release notes and in a follow-up wireframe document.

**Success criteria.** Users complete SMS onboarding in v0.1 while fully understanding that this is a preview path and that a sovereign migration is planned for v0.2.

---

## Screen 3c-next — SMS via the Twake SMS Companion Android app (v0.2 target)

**Status.** Design specification for v0.2. Not implemented in v0.1. Retained here so the v0.2 build starts from an aligned specification rather than a blank sheet. See ADR 0004 for the architectural rationale.

**Purpose.** Guide the user through installing the Twake SMS Companion on their Android phone and pairing it with their Twalk deployment. This replaces the v0.1 mautrix-gmessages path with a sovereign flow that keeps SMS on the user's phone and their Twalk server, with no third party in the middle.

**Trigger.** User tapped the SMS card on screen 3.

**Design intent.** SMS is a legally significant network (administrative notifications, banks, doctors). Twalk's SMS path must be self-contained: the messages must transit only between the user's Android phone and the user's Twalk deployment, with no cloud intermediary. The Twake SMS Companion is a small open-source Android app that Twalk provides for that exact purpose. It uses Android's official [SmsManager](https://developer.android.com/reference/android/telephony/SmsManager) API to send SMS, and registers as a Notification Listener plus SMS BroadcastReceiver to receive them. It publishes messages to a dedicated Matrix appservice room configured on the user's Twalk homeserver.

**Layout.**

- Header: back arrow, title "Connect SMS".
- Body top: informational card, calm tone: "To send and receive SMS through Twalk, install the Twake SMS Companion on your Android phone. It reads and writes SMS on your phone using the standard Android APIs, and forwards them to your Twalk server over an end-to-end encrypted Matrix session. No Google account is involved. No third-party cloud sees your SMS."
- Body middle: three-step flow.
  - **Step 1: Install the app.** Two side-by-side buttons: "F-Droid" (recommended) and "Google Play". Below, a large QR code that opens the F-Droid listing when scanned. Below that, a smaller link for advanced users: "Direct APK download (verify signature)".
  - **Step 2: Grant SMS permissions.** Screenshot walkthrough showing the two prompts the Twake SMS Companion asks for on first launch: SMS Read and Send permission, and Notification Listener access.
  - **Step 3: Pair the app with this Twalk deployment.** The Companion PWA generates a pairing QR code by calling a pairing endpoint on the Companion Gateway (`/api/channels/…` in the original sketch; the shipped vocabulary is `network`, so the path this is designed against will be `/api/networks/sms/pair`). The QR encodes a short-lived pairing token, the Twalk homeserver URL, and the appservice room id. Instructions: "1. Open Twake SMS Companion on your phone. 2. Tap 'Pair with Twalk'. 3. Scan this QR code."
- Body bottom: "Troubleshooting" link, and an explicit note: "Your Android phone must stay online for SMS to flow through Twalk. If your phone is off, incoming SMS will queue on your carrier and be delivered to the Twake SMS Companion when the phone comes back online."

**States.**

- *iOS user detected:* screen replaced by a friendly explanation, "SMS through Twalk requires an Android phone. iOS does not allow apps to read or send SMS on behalf of the user. If you have both an iPhone and an Android device, you can use the Android device as your SMS source." With a "Skip SMS for now" button that returns to screen 3.
- *Waiting for pairing:* QR visible with a countdown showing the pairing token expiry (10 minutes).
- *Pairing token expired:* QR replaced by a "Refresh" button.
- *Paired, first message received:* success animation, "Twake SMS Companion is now paired with your Twalk deployment. You are all set."
- *Failure:* explicit error messages for pairing token invalid, homeserver unreachable from the phone, or permissions denied on the phone.

**Pairing state.** Polled, not streamed: screens 3a and 3c settled that for every login this document describes, and there is no event stream on the Companion Gateway. On success, `bridge.status.changed.v1` with `network=sms`, `to_state=connected` (`bridge_id` prefixed with `bridge-sms-` to keep the taxonomy consistent even though this is not a Mautrix bridge).

**Notes.**

- The Twake SMS Companion is planned to be developed as a first-party LINAGORA open-source project, released on F-Droid and on the Play Store. Distribution on F-Droid is the primary channel and matches Twalk's sovereign stance.
- RCS support is out of scope for v0.1. Android does not expose a public RCS API to third-party apps; RCS remains a Google-controlled path through Google Messages. If a public RCS API becomes available, the Twake SMS Companion will add support.
- The Twake SMS Companion runs entirely on the user's phone. It does not use Firebase Cloud Messaging, it does not phone home to LINAGORA, and it does not require Google Play Services. Wake-ups are handled by Android's SMS broadcast intents (system-level events, delivered even when the app is in the background).

**Success criteria.** Android users complete pairing in under 5 minutes. iOS users get a clear message and a graceful skip. The SMS path involves no third party beyond the user's carrier.

---

## Screen 3d — Connect an existing Matrix account

**Purpose.** Allow users who already have a Matrix account to bring it into Twalk without creating a new identity. This is the sovereign path preferred by advanced users, and [ADR 0009](../architecture/adr/0009-matrix-is-a-network.md) makes what it observes a network of its own: a room with no `m.bridge` state event and no ghost-prefixed sender is published with `network=matrix` rather than dropped.

**Trigger.** User tapped the Matrix card on screen 3. Screen 1's second entry point into this screen went with the pairing link it was part of.

**Overruled — "on any homeserver". In v0.1 this screen watches only an account on the homeserver this deployment drives, and it says so before asking for a credential.** [ADR 0020](../architecture/adr/0020-observing-a-foreign-homeserver-is-a-device-of-the-users-account.md) is the record. The invitation model this screen was designed around is not merely unimplemented for a foreign homeserver, it **cannot work** on the deployments Twalk targets: `@sensor:<deployment>` would have to *join* a room hosted elsewhere, which needs federation in both directions — a public name, a certificate, a server the other side will talk to — and a deployment reached on loopback through a tunnel is federatable by nobody. Found live against a real corporate account ([#138](https://github.com/linagora/twalk/issues/138)): `.well-known` delegation, SSO through the organisation's identity provider, the redirect back to a tunnelled origin and the room listing all worked, and only the last step could not land, *after* the owner had signed in and chosen a room. A screen that cannot do a thing must say so before it takes something from you.

So the shipped screen compares the homeserver the typed name resolves to against the one this deployment drives, **as base URLs and only where the delegation lands**, and refuses a foreign one with a card: "Your account is on `<wanted>`, and this Twalk deployment is on `<deployment>`. In this version Twalk can only watch an account on its own homeserver — so nothing is asked of you here, because signing in would not help", plus what is missing and what replaces it. Two comparisons were tried and were wrong, and it is worth not repeating them: the typed name against the deployment's server name refuses a domain that delegates to this very deployment, and a typed *address* against a *server name* is the mistake [#130](https://github.com/linagora/twalk/issues/130) cost an evening — `127.0.0.1:8009` is where a homeserver answers, `test.twalk` is who it is. When the browser does not know its own deployment's base URL there is no question to ask and nothing is refused; the failure then arrives at the invitation, as it did before.

**Stage two, and the shape it has.** An account on any other homeserver is watched by **a device of that account** ([ADR 0020](../architecture/adr/0020-observing-a-foreign-homeserver-is-a-device-of-the-users-account.md), [ADR 0025](../architecture/adr/0025-twalk-acts-as-the-user-through-a-device-of-their-account.md)): created by the Companion during onboarding, holding a session on the user's own homeserver, its access token going straight into the Sensor's store and never through the Companion Gateway. The device is cross-signed like any other of the user's devices, appears in their device list, and is revocable from any Matrix client without Twalk's involvement. That is the same mechanism that posts an approved reply as the user, so the project grows one observation story rather than two that drift apart — and it asks nothing of the world: no federation, no invitation, no unfamiliar account joining a colleague's room.

**Layout.**

- Header: back arrow, title "Connect your Matrix account".
- Body top: caption saying Twalk watches the existing account rather than creating one, and that the Sensor is invited into the rooms the user ticks, changing nothing about their other Matrix client sessions.
- The homeserver field: **empty on a first visit**, accepting *either a server name or an address* (`.well-known` delegation is what Matrix has for exactly this, and `linagora.com` is what a person can recite). It used to arrive pre-filled with the Twalk deployment's own homeserver — the one account this screen is not for — and the consequence was not cosmetic: the screen reads the login flows of whatever is in the field, so the local deployment's password form was offered to an owner whose real account signs in through their organisation's identity provider ([#124](https://github.com/linagora/twalk/issues/124)). The only value ever offered is the homeserver of an account this browser has already connected here.
- Body middle: the login methods the homeserver actually advertises, read after the field resolves.
  - **Sign in with password**, when the homeserver offers it.
  - **Sign in with SSO**, with one button per identity provider the homeserver names, and a line saying which return address the homeserver will be asked to send the user back to — "a homeserver that only allows return addresses it knows will refuse this one until its administrator adds it", which is a real failure and not a hypothetical one.
  - A homeserver offering neither is its own answer: "This homeserver offers no sign-in method the Companion can drive. Sign in from your usual Matrix client, then come back."
- Body bottom, after signing in: the room list, **read in the browser** and sent nowhere. Only the ids the user ticks reach `POST /api/bootstrap/rooms`, which is the acceptance criterion this screen exists for. A search field filters it, and the bulk control acts on **what the filter is showing, never on the whole account**: on a work account this list is colleagues' private conversations, and a control that ticks all of them in one click is how sixty people end up observed without anyone deciding to ([#122](https://github.com/linagora/twalk/issues/122)).

**Overruled — "Sign in with QR code (recommended)".** MSC4108 sign-in is not offered in v0.1. It carries the user's encryption secrets across from another client, which needs cryptographic machinery these screens deliberately do not load, so the screen *names* it — "signing in by QR code from another Matrix client arrives with a later version" — rather than showing a control that would do nothing.

**Overruled — the Sensor verification stage, and the appservice on someone else's homeserver.** The original second stage asked the user to verify a Sensor *device* with an emoji sequence, and the note below it said the Companion would provision the Sensor on the user's homeserver as an appservice, falling back to the Twalk homeserver if that were refused. Neither is the model any more. For rooms on the homeserver this deployment drives, the Sensor is an account of this deployment and is simply **invited** into the rooms the user ticked (`POST /api/bootstrap/rooms`, which takes the user's Matrix access token as a parameter of that one call and forgets it when the call returns). Asking for administrative rights on somebody else's homeserver was rejected outright in [ADR 0020](../architecture/adr/0020-observing-a-foreign-homeserver-is-a-device-of-the-users-account.md). The emoji verification is not gone in spirit: cross-signing is named in both ADR 0020 and ADR 0025 as **the hard part** of stage two, where the device to be signed is one of the user's own and the verification runs against a homeserver the deployment does not control — and where, ADR 0025 adds, no automation should shortcut it by asking for a recovery key. When that screen is designed it will be designed there, against a real SSO-only account, rather than inherited from here.

**States.**

- *Nothing typed yet:* the field, and no login method — the screen asks the homeserver before it offers anything.
- *Resolved to a foreign homeserver:* the refusal card above, no credential asked for.
- *Resolved, flows read:* password and/or SSO, per what the homeserver advertises.
- *Signed in:* the room list, with a search field and a per-room tick; nothing selected by default.
- *Inviting:* one outcome per room, so one refusal does not discard the other decisions.
- *Two refusals, two places:* this screen has two actions with a page between them, so signing in and listing answer at the top controls, and the invitation answers **against its own button** — which on an account with a hundred rooms is three thousand pixels further down. The owner who reported "nothing happens" had been told, at the top of a page they were at the bottom of ([#139](https://github.com/linagora/twalk/issues/139)).
- *An SSO round-trip that lost its way:* "this sign-in could not be completed, because the browser no longer knows which homeserver it started at. Nothing was sent anywhere."

**Notes.**

- The Matrix session is made in this page, against the user's own homeserver. Its access token stays here, crosses exactly once as the parameter of `POST /api/bootstrap/rooms`, and is written to no store: a reload loses it and the user signs in again rather than finding it lying about ([ADR 0011](../architecture/adr/0011-gateway-authenticates-with-matrix-openid.md)).
- A room's name is unencrypted state, so it is readable without the crypto stack. What is not readable is the name of a room that has none — a direct message, which every Matrix client computes from its members' profiles, needing member state this listing deliberately does not fetch. Such a room is shown by what is actually known about it and labelled as such, rather than rendered blank.
- The deployment keeps **no record** of which rooms were invited: the token that did it was forgotten as soon as the invitation was sent. That is why screen 4 has to *ask* whether the user connected a Matrix account rather than reading it back.
- For an existing Matrix account, there is no WhatsApp/Signal/SMS bridge configured automatically. The user still goes through screens 3a, 3b, 3c on demand.

**Success criteria.** Advanced Matrix users bring their own account without friction, and a user whose account this version cannot watch learns that before they type a password.

---

## Screen 4 — Persona activation: Assistant

**Purpose.** Introduce the concept of personas and activate the first one (`assistant`) with a safe default.

**Trigger.** The user reaches this screen after screen 3, whether or not a network was connected — a screen that says "connect a network first" is more use than a screen the user cannot get to. The networks it offers are the ones `GET /api/bridges` says this deployment has connected.

**Layout.**

- Header: back arrow, title "Activate your first agent", step indicator "Step 2 of 3".
- Body top: card with the persona name "Assistant" and a one-sentence description: "Your assistant reads the messages arriving on the networks you connected, and suggests replies for you to review. It never sends anything without your approval."
- Body middle: two rows, both locked on, each saying why:
  - "Read incoming messages" — "On the networks you choose below, and on no others." Always on: an assistant that reads nothing has nothing to suggest.
  - "Suggest replies for your review" — "A suggestion waits for you. Nothing leaves your deployment until you approve it." Always on: reading and suggesting are one decision in this version — the assistant is active, or it is paused.
- **What it may read** — a tick per connected network. "The assistant is activated on exactly the networks you tick here. Connecting another network later leaves it inactive there until you say otherwise." A Matrix account gets a hint rather than a certainty, because the deployment kept no record of the rooms the Sensor was invited into (screen 3d). A Gateway that could not answer means the screen says so rather than letting the user activate blind.
- **Who it may read, by default** — "Every contact starts at pending. The assistant reads nothing from someone until you decide about them, and there is no rule that grants anyone automatically — this version imports no address book."
- Body bottom: what activating means on a deployment that runs no agent platform — the decision is recorded and published, and a runtime honours it from the day it starts. (The shipped copy still states this as though Hermes did not exist; it landed since, so that sentence is the Companion's to update, not this document's.)
- Primary CTA: "Activate the assistant".

**Overruled — "Auto-send replies you have approved once".** Removed by the design review of 2026-09-17 ([#74](https://github.com/linagora/twalk/issues/74)). It was the only control in the product that would let an agent send without human approval, and it sat two lines below the same card's promise that the assistant "never sends anything without your approval". If it is ever wanted it needs its own ADR and its own design, not a toggle in an onboarding flow.

**Overruled — active hours.** Removed by the same review. A time window is a consent rule to be enforced at runtime, audited, and made robust against clock and timezone changes; the v1.0 milestone already plans consent policies with time windows. A greyed-out or cosmetic control in a privacy screen suggests a protection that does not exist.

**Overruled — the consent default's address-book rule.** `pending` for **every** contact, full stop. The original default granted contacts already in the user's phone book; v0.1 imports no address book, and adding one is a personal-data path of its own with its own permissions, its own storage and its own explanation.

**Why those three could go without anything replacing them.** Activating a persona **is** a consent decision ([ADR 0013](../architecture/adr/0013-persona-activation-is-a-consent-decision.md)): one `POST /api/consent/decisions` with a `persona` subject, scoped to the networks ticked here. There is no persona control API and the Companion Gateway never manages persona processes, so this screen deliberately offers nothing that could not be written as that one decision — which is the test each removed control failed. The same ADR is why activation never spreads: connecting a new network leaves every persona inactive on it until the user says otherwise, because an agent that starts reading a network the user has just connected, unasked, is the precise behaviour this project exists to prevent. And pausing a persona is the revocation of that decision: the persona still runs and receives nothing — starved, not stopped.

**States.**

- *No network connected:* the screen says so and offers "Connect a network" rather than an activation that would read nothing.
- *The bridge list unreadable:* "Twalk could not read which networks this deployment has connected, so this screen cannot say what the assistant would be allowed to read. Try again rather than activating it blind." An unanswered `GET /api/bridges` used to render as *no network is connected yet* — a statement about the user's deployment made from no information at all, and one a user acts on by re-connecting a network that was never disconnected ([#111](https://github.com/linagora/twalk/issues/111)).
- *Ready:* CTA enabled.
- *Activating:* CTA becomes a spinner.
- *Success:* "Your assistant is now watching `<networks>`. It is activated on those networks and on no others." Continue to Screen 5.
- *Refused:* the decision was refused, and nothing changed.

**CloudEvents emitted.** On activation, the Companion Gateway emits a `consent.state.changed.v1` event with `subject.type=persona`, `subject.id=assistant`, `old_state=unset`, `new_state=granted`, and `scope.networks` naming exactly the networks ticked.

**Two decisions this screen does not carry, and should.** Both are recorded in an ADR and neither has a screen anywhere in this document — a gap here rather than in the ADRs.

- **A model must be configured, or the persona refuses to start** ([ADR 0015](../architecture/adr/0015-no-default-llm-configured-through-the-companion.md)). Twalk ships no LLM and has no default: nothing is ever sent to a model the operator did not name. The configuration — an OpenAI-compatible base URL, a model name, an optional credential, a free-form object of provider parameters — is held by the Companion Gateway (`/api/settings/model`, [#98](https://github.com/linagora/twalk/issues/98)) and injected by the Hermes runtime into each persona, never fetched by a persona. So "activate the assistant" is not sufficient for a suggestion to appear, and no screen here tells the user that or lets them fix it. A credential the operator supplied as a file wins over one set from the browser, which is the reference deployment's own shape.
- **A drafted reply discloses itself to the contact** ([ADR 0019](../architecture/adr/0019-a-persona-discloses-itself-to-the-contact.md)). Every reply a persona drafted carries a short line telling the recipient it was written with the user's assistant and sent by the user. It is on by default, the user may turn it off, and turning it off is recorded as a deliberate act with a timestamp rather than a silent preference — which is exactly the kind of control this screen is where the user would look for. Since [#121](https://github.com/linagora/twalk/issues/121) it is built, but not here: the switch and its dated record are a card on the settings screen at `/settings`, beside the language card, and the approval screen shows the sentence fixed under the editable reply — or says the switch is off, and since when. This screen still does not mention it, which is the remaining gap.

**Success criteria.** User understands what the assistant will do, is not surprised by any behaviour later, and reaches Screen 5.

---

## Screen 5 — Home dashboard

**Purpose.** The recurring home screen the user returns to. Shows the current health of the system, and is the one place that says plainly what this deployment can and cannot report about itself.

**Trigger.** User completed Screen 4, or opens the Companion again after any first-time flow.

**Layout.**

- Header: title, refresh control.
- Body top: system status card
  - Overall health indicator ("All systems operational", "Something needs you", or "Almost there" with what is missing).
  - Networks section: one row per configured bridge, with a state, the network name and a "Manage" arrow. The states are the bridge's own vocabulary — connected, connecting, session expired, login failed, not connected, state unknown.
  - Agents section: one row per persona, with an active/paused toggle, the networks it is active on, and what pausing means.
  - Messages carried: a **count**, or "not counted on this deployment".
- Body middle: recent activity feed — bridge state changes, consent decisions, persona activations and pauses, device sign-ins and revocations. Each row is a chip with an icon, a short label, and a relative time. Plus the standing explanation of what the feed is not: "Operational events only: bridges, consent and agents, plus a count. Twalk does not keep a list of who writes to you, so this screen cannot show one."
- Body: **your devices** — one row per device that has signed in, with its last-seen time and a Revoke action; revoking the current device signs the user out here, and says so before it does ([ADR 0011](../architecture/adr/0011-gateway-authenticates-with-matrix-openid.md)).
- Body bottom: "Add a network", "Activate your assistant" when it has never been activated, and a Messagr card that says pairing is not available in this version.
- Footer: the version this deployment answers with.

**Overruled — the activity feed's sender identities.** The feed as designed showed "the last 10 events on the bus (received messages …)" with who sent them. That feed is **the list of who writes to you**, rendered on a home screen that gets unlocked in public, in a product whose argument is sovereignty. The design review removed it ([#74](https://github.com/linagora/twalk/issues/74)): the feed carries **operational events only** — bridge state, consent decisions, persona activity — plus a **message count**. It is not a rendering choice, it is what the system holds: the Companion Gateway deliberately stores no such list ([#54](https://github.com/linagora/twalk/issues/54) keeps a contact's Matrix ID, its network and two timestamps, and no body, display name or `network_identifier`) and the Companion caches none ([#66](https://github.com/linagora/twalk/issues/66)). The rule is enforced at the seam, in `$lib/dashboard/model.ts`, where it is tested, rather than in the markup — and the same reduction happens to every other read this screen makes, so no component here could render a contact or a persona's words by accident.

**Overruled — the merged Server-Sent Events stream.** There is no `/api/events/stream` on the Companion Gateway and no subscription anywhere in the Companion: this screen **polls** every 15 seconds. The wireframe also named `agent.suggest.produced.v1`, which is not a contract type — the domain is `persona.*` (`persona.suggest.produced`), and "agent" is not the word for a persona in this project (`CONTEXT.md`).

**Overruled — the bottom navigation bar.** Home, Bridges, Personas, Consent, Settings was never built. When it was decided, two of the five had no screen at all (the consent inbox was v0.2, and the model and language settings lived on the Gateway with no Companion screen); both exist now — `/consent` since [#170](https://github.com/linagora/twalk/issues/170), `/settings` since [#101](https://github.com/linagora/twalk/issues/101) — and the dashboard links to them, as it does to the other three, which are steps of a journey. A tab bar was worse than links that work when two of its rooms were empty, and nothing since has argued for one.

**States.**

- *All green:* calm layout, muted colors.
- *Bridge session expired:* amber banner at the top, "Your `<network>` session expired. Reconnect to keep receiving messages from it." with a "Reconnect" button.
- *Pending consent decisions:* a chip with a count, "3 consent decisions waiting for you" — a real number, read from `GET /api/contacts/pending`'s `total`. Its `contacts` array is dropped at the seam and never reaches a component. The chip **does not open an inbox**, because there is none in v0.1: it says where the decisions are not, instead of pretending to lead somewhere, and adds that granting or revoking a whole network decides for everyone on it at once.
- *Replies waiting for approval:* a chip with a count from `GET /api/suggestions`, linking to `/approvals`. The count can be a floor rather than a total when the server read back only part of the stream, and it says so. What the assistant wrote is **not** shown here: "this screen gets unlocked on a train."
- *Deployment unreachable:* the screen says what it is showing is the last answer the deployment gave, rather than drawing stale data as current.

**Three things this screen refuses to imply.** Each is a fact about the deployment rather than a design choice, and each was drawn confidently in the wireframe.

- **The bridge dots read the last login each bridge holds, not a heartbeat**, and **nothing reports the time of the last message**. [#56](https://github.com/linagora/twalk/issues/56) landed the producer — bridges push their state and the Gateway publishes `bridge.status.changed.v1` — but no read on this origin serves those events to a browser, so both facts are said on the screen ("the time of the last message is not reported by this deployment yet") rather than drawn as a confident green with a plausible timestamp.
- **A bridge the Gateway could not reach is unknown, not disconnected.** One bridge being down costs the others nothing.
- **A persona's activity is a decision, not a heartbeat.** Pausing is explained as *starved, not stopped* ([ADR 0013](../architecture/adr/0013-persona-activation-is-a-consent-decision.md)).

**What the message count counts.** The owner is not a contact, so their own traffic is not in the same family as a contact's: a message the user sent from their own phone is `outbound.message.sent`, their own reaction is `outbound.reaction.added`, both with the owner's Matrix ID as the subject and no `consent` extension at all, and their **presence is not published** ([ADR 0018](../architecture/adr/0018-the-users-own-messages-are-their-own-event-type.md), [ADR 0021](../architecture/adr/0021-the-owner-is-never-a-contact-on-any-event.md)). A dashboard that counts "messages" must therefore say which — and the count exists on this screen precisely because it is the one thing about the traffic that can be stated without naming anybody.

**Success criteria.** User can, in under 5 seconds, tell whether Twalk is working normally. If not, they know exactly what to do next — and they are never told something about their deployment that the deployment did not actually report.

---

## Screen 6 — Approving a suggestion

**Added after the fact.** This document had no approval screen, which is the largest thing missing from it: screen 4 promised an assistant that "suggests replies for your review", and no screen reviewed one. It is shipped at `/approvals` ([#100](https://github.com/linagora/twalk/issues/100)) and described here so the document covers the loop it promised.

**Purpose.** Read what a persona proposed, edit it if wanted, and perform the one deliberate act that sends it.

**Trigger.** The approval chip on screen 5, or the route directly.

**Layout.**

- Header: title "Replies waiting for you", and "Your assistant proposes; you decide. Nothing is sent until you say so."
- One card per suggestion: which persona proposed it, when it was written, when it goes stale, which attempt it is for that message, and the proposed reply in full.
- Per card: "Approve and send", "Edit before sending", "Refuse".
- Empty state that distinguishes *nothing to approve* from *something wrong*: "No persona has proposed a reply in the stretch of the event stream this server reads", plus what that looks like when no assistant was activated or no agent platform runs here.

**Four decisions are the shape of this screen.**

- **It cannot name the contact, and says so on the screen.** `GET /api/suggestions` carries the message a suggestion answers as an id and a type and nothing else, because naming the sender means opening their message — and an excerpt belongs to the author of the quoted message rather than to whoever sent the event carrying it ([ADR 0012](../architecture/adr/0012-revoked-consent-reduces-publication.md), [#110](https://github.com/linagora/twalk/issues/110)). So the row has no member a contact could go in, there is no second read anywhere, and the copy tells the user it is a decision rather than a defect: "Your server lists what your assistant wrote, and identifies the message it answers by its reference alone — never by the sender, never by a quote of what they said." Whether that trade is the right one is open ([#160](https://github.com/linagora/twalk/issues/160)), and this screen is the evidence that question was waiting for: one suggestion reads fine alone, and *two at once* are indistinguishable, which is the real cost.
- **Approval is one act at a time, and the Companion Gateway performs it.** `POST /api/approvals` reads the suggestion and its trigger from the bus, refuses an expired suggestion, refuses one whose trigger was never consented, and refuses one whose sender's consent is no longer `granted` **at that moment** — a read of the Gateway's own journal rather than a question asked of another process, which is why the endpoint lives there and not on the Hermes runtime ([ADR 0022](../architecture/adr/0022-the-approval-api-lives-on-the-companion-gateway.md)). There is no "approve everything", nothing is ticked in advance, and no keystroke sends: "Nothing is sent by typing here, and no key sends it."
- **Every refusal names a cause and a next step.** The Gateway's refusal codes each have a sentence and a remedy, nothing renders as a spinner that never ends, and a publication that landed but was not recorded says the reply *did* go out. A reply the Gateway recorded and the bus never acknowledged is listed with its text and a "Send it again" that republishes under the same deterministic id — it cannot be sent twice, whatever happens.
- **Refusing is local, and the copy says so.** No route on this origin refuses a suggestion, and none could without a second store the project refuses to have. So "Refuse" hides the row in this browser: the assistant is not told, nothing is recorded on the server, and the suggestion goes stale on its own.

**What a sent reply does next.** The approval publishes `persona.reply.approved.v1`, whose `source` still names the persona whose suggestion it was rather than the Gateway that published it. The Sensor then posts it into the portal room **as the user**, through a device of the user's own Matrix account — a bridge relays to its network only what the logged-in user's own account sends ([ADR 0025](../architecture/adr/0025-twalk-acts-as-the-user-through-a-device-of-their-account.md)). The screen says where the publication landed, so "did my reply go out?" is never a spinner.

**Language.** A suggestion is written in the language of the **message it answers**, not the user's ([ADR 0016](../architecture/adr/0016-a-reply-follows-the-conversation-not-the-user.md)) — a French user answering an English-speaking contact in French has been handed something they cannot use. The interface around it is in the user's own language; the two are different axes, and this screen is where they meet.

**Success criteria.** A user can tell what a reply says and what sending it will do, and nothing about the screen makes sending accidental.

---

## Screen 7 — Choosing which conversations are observed

**Shipped** at `/networks/conversations`, reachable from each connected network's card ([#143](https://github.com/linagora/twalk/issues/143)). The wireframes never asked which conversations the Sensor may read, and the answer they implied — all of them, as soon as a network is connected — is the shape of the worst defect this product has had. This section records the decision the screen was designed against rather than the old assumption. The register is live ([#105](https://github.com/linagora/twalk/issues/105)).

**What went wrong.** A freshly connected network published nothing at all while every component reported itself healthy. A bridge builds a portal room **lazily**, as each conversation becomes active — eighteen appeared on the reference deployment between 04:54 and 13:24 — and invites only the user, so nothing ever put the Sensor inside one and the deafness grew on its own. A fix aimed at the rooms that exist when a network is connected would have been right for about an hour ([ADR 0024](../architecture/adr/0024-a-portal-room-is-observed-by-invitation-per-conversation.md)).

**What the decision fixes about this screen's design.**

- **Nothing is observed by default.** Those eighteen rooms held some 1,300 memberships, a family member and a 246-member association under one mechanism. The mechanism is the Companion Gateway's and the policy is the user's, **one conversation at a time**.
- **The list is a projection of the homeserver, not a store.** `GET /api/portals` reads each bridge's portal rooms as that bridge's own bot, and whether a conversation is observed is the Sensor's own `m.room.member` event, asked again on every read — so a reconnection that rebuilt every portal is simply the next answer. A conversation's name and member count are read for the request and forgotten with the response; no message content is read at any point.
- **The screen must be able to state a number.** "The Sensor is outside 17 of your 18 conversations" is a sentence the deployment can now say (`summary` in the answer, `twalk_companion_gateway_portal_rooms` on `/metrics`), with **every** configured bridge listed readable or not — so it never quietly means "…of the 18 I could see".
- **Both directions, one mechanism.** `POST /api/portals/observation` invites the Sensor into the named rooms, or removes it, as that bridge's own bot. One room failing does not fail the others.
- **A portal stuck at `invited` is its own diagnosis:** the Sensor refused the inviter, which is `SENSOR_ALLOWED_INVITERS` not naming that bridge's bot.

**What the shipped screen adds to that.**

- **A third unit of decision.** Consent is per contact, persona activation is per network, and observation is **per conversation**. For `maria` a contact and a conversation are the same thing and consent-per-contact works; for a 246-member association nobody adjudicates 246 people one by one. The two compose rather than compete: an unwatched conversation produces nothing, and inside a watched one each sender's consent still governs what is published about them ([ADR 0012](../architecture/adr/0012-revoked-consent-reduces-publication.md)).
- **Grouped by what the network says, not by what the screen guesses.** Each portal carries the network's own identifier for the conversation (`network_conversation_id`, the `m.bridge` `channel.id`, passed through the Gateway untouched), and its suffix says the kind: `@lid` and `@s.whatsapp.net` are one person, `@g.us` a group, `@newsletter` a broadcast. A conversation whose bridge wrote no identifier gets a section that says the kind was not stated — never one inferred from the member count, which is evidence about a conversation's size and not its type.
- **Communities are grouped by name, and the screen says so.** A WhatsApp community is, at the network level, a set of ordinary `@g.us` groups and nothing says which groups form one. What the register can see is what the owner saw: a community arrives as several groups whose names contain one another — `Communauté CKCP` twice in the same minute at 109 members and 6, `XVDSI` and `XVDSI - General`. So they are clustered by name, no row is labelled the parent, and each keeps its member count and its network address, which is what tells two rows called `XVDSI` apart.
- **The consequence is stated before the tick takes effect.** A community's control says how many people it covers in its own label, the pending decision is costed in people against the whole account rather than against what the search left on screen, and past a crowd it cannot be applied until the number has been acknowledged. A number is not a warning, and a warning nobody reads is not a decision ([#122](https://github.com/linagora/twalk/issues/122)).
- **The bulk control is the room chooser's**, scoped to the filter and counted — "Select the 4 shown", never "select all" ([#137](https://github.com/linagora/twalk/issues/137)), one implementation for both screens.

**Success criteria.** A user can see how many of their conversations Twalk is reading, change that one conversation at a time, and never discover after the fact that a 246-member group was observed because a screen selected everything for them.

---

## Design system notes

- **Colors:** Twake teal (`#00A896`) for primary actions, amber (`#F4A261`) for warnings, red (`#E63946`) for errors, green (`#2A9D8F`) for success, gray scale for neutrals. Dark mode uses inverted contrast with the same accent colors.
- **Typography:** Inter for UI (weight 400 for body, 500 for labels, 700 for headings), JetBrains Mono for the recovery key and any technical strings. Base size 16 px, line height 1.5.
- **Spacing:** 8 px baseline grid. Component paddings are multiples of 8.
- **Motion:** transitions of 200 ms ease-out for state changes, 400 ms for screen transitions. Reduced motion mode respected via `prefers-reduced-motion` media query.
- **Icons:** Lucide icon set, 24 px default, 20 px in dense contexts.

**Shipped.** These values are the ones the Companion's tokens hold (`companion/src/lib/styles/tokens.css`), and they are **transcribed rather than imported**: no consumable Messagr package exists to take them from (`@linagora/twake-mui` is React and MUI), so they live in one file, ready to be replaced by such a package when there is one. Every rule in the app reads a token, because a component that hard-codes `#00A896` is a component that will not follow the design system into dark mode. Dark mode follows the system by default and can be pinned with `data-theme` on the root element. Lucide reaches the app through one indirection module rather than being imported per component.

---

## Accessibility

- All interactive elements have visible focus states with a 2 px accent-colored ring.
- All text achieves WCAG AA contrast on both light and dark backgrounds.
- All CTAs have descriptive `aria-label` when their text alone is ambiguous.
- QR codes have an accompanying text alternative for screen readers: "Scan this code with WhatsApp Settings > Linked Devices".
- Form errors are announced via `aria-live=polite` regions.
- Keyboard navigation covers all screens; no interaction requires touch.

---

## Localization

**Corrected — five languages, not two, and the user's language is stored rather than sniffed.** The Companion ships in **English, French, Italian, Spanish and German** ([ADR 0016](../architecture/adr/0016-a-reply-follows-the-conversation-not-the-user.md)). Screen copy uses ICU MessageFormat placeholders for all interpolated values (username, domain, counts), and the catalogues live at `companion/src/lib/i18n/<locale>.json`, each carrying a header stating whether a native speaker has reviewed it — English and French are reviewed; Italian, Spanish and German are not, and that status is recorded in the catalogue and in `CONTRIBUTING.md` rather than shown to a user who could do nothing about it. A key missing from a catalogue falls back to English at runtime, and a test fails when a catalogue's keys have drifted from English's, so an incomplete contribution is acceptable while a silent divergence is not.

What replaces "inferred from `navigator.language`" is not a browser setting but **a stored preference**: the user's native language is set in the Companion, held by the Companion Gateway and injected into each persona by the Hermes runtime alongside the model configuration. It has to be stored, because a persona runs in a container and cannot read `navigator.language` — and it governs the interface, the explanations, and *only* the fallback a persona uses when it cannot tell what language the message it is answering was written in. It never governs the text sent to a contact: a suggestion follows the conversation. Prompts sent to a model stay in English, which is a separate mechanism from the interface's translation and needs no translated string.

---

## Out of scope for v0.1

- Sovereign SMS onboarding via the Twake SMS Companion Android app (design retained in screen 3c-next above, implementation in v0.2, see ADR 0004).
- Telegram and Discord onboarding screens (deferred to v0.2).
- Multi-persona management screens (deferred to v0.2).
- Consent inbox with searchable contact list (deferred to v0.2).
- Consent policies with time windows and rule builder (deferred to v1.0).
- Bridge diagnostic deep-dive with logs and metrics (deferred to v1.0, may live in Buzz instead).
- Onboarding tour or product tour (deferred, decided only after v0.1 user feedback).
- Native mobile shell (deferred, likely never — see ADR 0002).
- Auto-sending a reply without human approval (struck from screen 4 by the design review; it needs its own ADR and its own design before it is a feature at all — [#74](https://github.com/linagora/twalk/issues/74)).
- A model and language configuration screen — the Gateway holds both ([#98](https://github.com/linagora/twalk/issues/98)) and the Companion's `/settings` ([#101](https://github.com/linagora/twalk/issues/101)) is that screen; what this list kept is that ADR 0015 means a deployment without a model produces no suggestions, and the screen says so rather than hiding it.
- The persona disclosure control ADR 0019 requires — built by [#121](https://github.com/linagora/twalk/issues/121) as a card on the settings screen, with the sentence shown fixed on the approval screen for a suggestion still to be decided (a sent, expired or approved row carries no line, since the record does not say under which switch it went out); what remains is that screen 4 says nothing about it.
- The per-conversation observation chooser (decided and specified in screen 7 above; the Gateway's half is in, the screen is [#143](https://github.com/linagora/twalk/issues/143)).
- Telling a contact their conversation is processed at all, which is a different question from a persona's own disclosure and is genuinely undecided ([#122](https://github.com/linagora/twalk/issues/122), residual risk 14 of [`security-model.md`](../architecture/security-model.md)).

**A note on ADRs 0001–0004.** They are referenced from this document and from the README and are **not written**: the numbers are reserved, and ADR 0005 is the first ADR recorded in this repository. Two of the references above are load-bearing and one of them is now stale. ADR 0004 (the Twake SMS Companion) is design intent for v0.2 and nothing has overruled it. ADR 0003, as this document summarised it — an appservice registration for the Sensor account on the user's own homeserver — is **overruled** by [ADR 0020](../architecture/adr/0020-observing-a-foreign-homeserver-is-a-device-of-the-users-account.md) and [ADR 0025](../architecture/adr/0025-twalk-acts-as-the-user-through-a-device-of-their-account.md): asking for administrative rights on somebody else's homeserver was rejected, and what watches an account elsewhere is a device of that account. The references are left as plain text rather than as links to files that do not exist.
