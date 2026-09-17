# Twalk Companion — Wireframes v0.1

This document describes the key screens of the Companion PWA for the v0.1 milestone (end of 2026). It is intended as design input for the frontend implementation, and as validation of the user experience before development starts.

v0.1 ships onboarding for **four channels**: **WhatsApp**, **Signal**, **SMS via mautrix-gmessages (proof of concept)**, and **an existing Matrix account** (bring your own account, without a fresh registration). Each channel has its own dedicated screen because the login mechanism, the failure modes and the trust story differ substantially between them.

**On the SMS path in v0.1 vs v0.2.** v0.1 uses [mautrix-gmessages](https://docs.mau.fi/bridges/go/gmessages/authentication.html) as a proof-of-concept SMS bridge. This lets us prove the end-to-end pipeline (SMS observed, published on the bus, surfaced to the assistant persona, replied to and sent back) without committing to a mobile release cadence in v0.1. The trade-off is transparent to the user: SMS transits through Google Messages Web infrastructure and requires a Google account. v0.2 replaces this path with the sovereign first-party [Twake SMS Companion](../architecture/adr/0004-twake-sms-companion-first-party-app.md) Android application published by LINAGORA on F-Droid. Both flows are described below (screen 3c for v0.1, screen 3c-next for v0.2).

Wireframes are described textually rather than visually to keep the document versionable in the monorepo, reviewable in pull requests, and translatable to any design tool without ambiguity. Each screen documents its purpose, layout, interactive elements, states, and success criteria.

Design tokens (colors, typography, spacing) follow the Messagr design system, itself aligned with the Twake product family. All screens are designed mobile-first (viewport 375 to 428 CSS pixels wide), with responsive extensions for tablet and desktop.

---

## Channel matrix (bridging strategy for v0.1)

| Channel | Bridge | Login mechanism | Notable constraints | Milestone |
|---|---|---|---|---|
| WhatsApp | mautrix-whatsapp | QR code, unofficial WhatsApp Web multi-device protocol via [whatsmeow](https://github.com/tulir/whatsmeow) | Personal (free) account works; account ban risk exists and must be disclosed to the user. See channel screen 3a. | v0.1 |
| Signal | mautrix-signal | QR code, secondary device pairing via libsignal | Primary device (phone) must stay online. Group history is not backfilled. | v0.1 |
| SMS (PoC) | mautrix-gmessages | Google account cookie login (QR is [deprecated by Google](https://docs.mau.fi/bridges/go/gmessages/authentication.html)); requires Google Messages app active on the primary Android phone | Not sovereign: SMS transits through Google Messages Web. Requires a Google account. Not available for iOS-only users. Explicitly framed as a proof-of-concept path; migrated to the sovereign Twake SMS Companion in v0.2. | v0.1 |
| Matrix (existing) | Direct login on the user's homeserver | Password, SSO, or QR sign-in from any Matrix client that supports [MSC4108](https://github.com/matrix-org/matrix-spec-proposals/pull/4108) (including Messagr) | No bridge needed. The Companion pairs to the existing account and the Sensor is invited into the user's already-encrypted rooms. | v0.1 |
| SMS (sovereign) | Twake SMS Companion (Android app by LINAGORA) using Android [SmsManager](https://developer.android.com/reference/android/telephony/SmsManager) and [Telephony.Sms](https://developer.android.com/reference/android/provider/Telephony.Sms) content provider, publishing to a dedicated Matrix appservice room | Requires an Android phone owned by the user; the phone must stay online for SMS to flow. No Google account or Google Play Services dependency. Not available for iOS-only users. Replaces the mautrix-gmessages v0.1 path. | **v0.2** |
| Telegram | mautrix-telegram | Phone number + code, or Telegram bot token | Standard Telegram authentication. | **v0.2** |
| Discord | mautrix-discord | OAuth login via Discord | Standard Discord OAuth. | **v0.2** |

---

## Screen 1 — Bootstrap: Welcome and homeserver

**Purpose.** First screen the user sees when opening the Companion for the first time. Establishes trust, states what Twalk does, and asks for the homeserver URL.

**Trigger.** Fresh install with no existing session in local storage.

**Layout (top to bottom).**

- Header: Twalk logo (32 px), title "Twalk Companion", subtitle "The command center for your sovereign messaging hub".
- Body: three-line description in the local language: "Twalk lets you bring all your conversations (WhatsApp, Telegram, Signal, Discord, Google Messages) into one place you own, watched over by agents you configure. This Companion sets everything up for you."
- Input field: "Your Twalk domain", placeholder "example.com", inline validation (must be a valid hostname).
- Primary CTA button: "Continue", disabled until the input is valid.
- Secondary link at the bottom: "I already have a Twalk account, pair with my other device" (leads to the dashboard pairing flow).

**States.**

- *Empty:* CTA disabled, no error.
- *Valid input:* CTA enabled.
- *Invalid input:* red micro-copy under the field, "Please enter a valid domain like `example.com`".
- *Loading (after Continue):* CTA replaced by a spinner, input disabled, subtitle changes to "Checking your Twalk deployment...".
- *Homeserver unreachable:* input turns red, error banner at the top "We could not reach `<domain>`. Check the URL or your internet connection." Retry available.

**Success criteria.** User understands what Twalk does in under 20 seconds, enters their domain, and reaches Screen 2.

---

## Screen 2 — Bootstrap: Create account and recovery key

**Purpose.** Provision the user's Matrix account on the homeserver, generate cross-signing keys, and get the user to save the recovery key.

**Trigger.** User completed Screen 1 with a reachable homeserver that supports self-service registration.

**Layout.**

- Header: back arrow, title "Create your account".
- Body top: field "Username", pre-filled with a suggestion based on the email if the homeserver supports SSO, otherwise empty. Validation: 3 to 30 characters, lowercase, digits, underscore, hyphen.
- Body middle: field "Password" and "Confirm password". Password strength meter. Minimum 12 characters, at least one letter and one digit.
- Body bottom: informational card explaining what a recovery key is: "Your recovery key unlocks your encrypted conversations if you lose this device. Twalk never sees it. Write it down or save it in a password manager. You cannot recover it later."
- Primary CTA: "Create my account".

**Second step within the same screen (after account creation).**

- Header changes to "Save your recovery key".
- Body shows the recovery key in a large, readable font (mono, tabular numerals): `AAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH` (32 chars grouped by 4).
- Copy button next to the key.
- Download button ("Save as PDF") that generates a printable one-page PDF with the key and human instructions.
- Checkbox: "I have saved my recovery key in a safe place".
- Primary CTA: "Continue", disabled until the checkbox is ticked.

**States.**

- *Username taken:* red under the field, "This username is already taken on `<domain>`". Suggest alternatives.
- *Homeserver rejects registration:* full-screen error, "This Twalk deployment does not allow new accounts. Ask your administrator." Contact link at the bottom.
- *Cross-signing keys generation:* short loading state (typically under 2 seconds).

**Success criteria.** User has an account, has saved their recovery key with intention (not just clicked through), and reaches Screen 3.

---

## Screen 3 — Channel picker

**Purpose.** After account creation, the user chooses which channels to connect. This screen is the entry point to the four v0.1 channel-specific flows (screens 3a, 3b, 3c, 3d). Users can connect one channel now and skip the rest for later. Telegram and Discord are shown as "coming in v0.2" cards so the user knows the roadmap.

**Trigger.** User completed Screen 2, or opens the Companion from the dashboard via "Add another channel".

**Layout.**

- Header: back arrow, title "Connect your first channel", step indicator "1 of 3 to finish setup".
- Body: card grid, one card per channel.
  - **Active cards (v0.1):**
    - **WhatsApp** — icon, name, subtitle "Personal or business account, via WhatsApp Web". Tap opens screen 3a.
    - **Signal** — icon, name, subtitle "Secondary device via Signal linked devices". Tap opens screen 3b.
    - **SMS via Google Messages (preview)** — icon, name, subtitle "Proof of concept via Google Messages. Sovereign path arrives in v0.2". Tap opens screen 3c. A discreet badge "Preview" is shown on the card. Grayed out with a helper tooltip if the browser reports an iOS user agent.
    - **Existing Matrix account** — icon, name, subtitle "Connect a Matrix account you already own". Tap opens screen 3d.
  - **Coming soon cards (v0.2, non-interactive, muted styling):**
    - **Telegram** — subtitle "Coming in v0.2".
    - **Discord** — subtitle "Coming in v0.2".
- Body bottom: link "Skip for now, I'll add channels later" (grayed until at least the account is created).
- Footer: informational chip "You can add or remove channels anytime from the dashboard."

**States.**

- *No channel connected yet:* skip link is subtle but not disabled.
- *At least one channel connected:* connected channels are marked with a green checkmark and a "Manage" secondary action.

**Success criteria.** Users understand that Twalk supports several channels and that they can start with just one.

---

## Screen 3a — WhatsApp login (QR code)

**Purpose.** Complete WhatsApp bridge login. This is the anchor experience for v0.1: if this works smoothly, the user believes in Twalk. If it stumbles, the user abandons.

**Trigger.** User tapped the WhatsApp card on screen 3.

**Layout.**

- Header: back arrow, title "Connect WhatsApp".
- Body top: visual hierarchy — a stylized illustration of two devices (a smartphone showing WhatsApp, this device showing Twalk). Below, the caption "Twalk logs into WhatsApp as an additional WhatsApp Web session. Your conversations stay end-to-end encrypted between WhatsApp and Twalk; Twalk just observes them."
- **Disclosure card (mandatory before showing the QR):** informational card, amber tone, with a heading "About WhatsApp accounts": "Twalk connects to WhatsApp via the same Web protocol as WhatsApp Web. It works with your personal (free) account and does not require a WhatsApp Business account. WhatsApp does not officially support third-party clients: in rare cases, accounts using Twalk may be temporarily suspended or banned by WhatsApp. Twalk minimizes this risk by respecting the Web protocol as strictly as possible, but the risk is not zero. You can dismiss this notice once, and it will not appear again on this device." Two buttons: "I understand, continue" and "Learn more" (opens a documentation page).
- Body middle: the QR code, generated on demand by calling `/api/bridges/whatsapp/login/qr` on the Companion Gateway. Refreshed every 60 seconds if not scanned.
- Body bottom: instructions in three numbered steps: "1. Open WhatsApp on your phone. 2. Go to Settings, then Linked Devices, then Link a Device. 3. Scan this QR code with your phone camera."
- Footer: "Troubleshooting" link that opens a bottom sheet with common issues, including "My phone must stay online at least once every 12 days for the link to remain active" and "WhatsApp allows up to 4 linked devices per account".

**States.**

- *Disclosure shown:* QR code hidden until the user confirms the disclosure. Confirmation is stored locally in the Companion PWA and does not expire.
- *Waiting for scan:* QR code visible, subtle countdown showing when it expires.
- *QR expired:* QR replaced by a "Refresh" button.
- *Scanned, waiting for confirmation:* QR replaced by a spinner and "Verifying your session...".
- *Success:* full-screen success animation (checkmark), "WhatsApp connected. Twalk is now observing your WhatsApp conversations." Continue button returns to screen 3.
- *Failure:* error banner with a specific reason (session revoked, phone offline, WhatsApp banned the session). Retry button. If the failure is a suspected ban, an escalated red card is displayed with next steps.

**Server-Sent Events.** The Companion subscribes to `/api/bridges/whatsapp/login/status` for real-time updates. On success, the Companion Gateway emits a `bridge.status.changed.v1` event with `from_state=starting`, `to_state=connected`.

**Success criteria.** 90% of first-time users complete this screen in under 3 minutes without opening the troubleshooting sheet, and are aware of the ban risk before they scan.

---

## Screen 3b — Signal login (QR code, secondary device)

**Purpose.** Pair Twalk as a secondary device on the user's Signal account, using the standard Signal linked devices mechanism.

**Trigger.** User tapped the Signal card on screen 3.

**Layout.**

- Header: back arrow, title "Connect Signal".
- Body top: caption "Twalk becomes a secondary Signal device on your account, just like Signal Desktop. Your primary phone stays required for account recovery."
- Body middle: the QR code generated by mautrix-signal via `/api/bridges/signal/login/qr`. Refreshed every 60 seconds.
- Body bottom: instructions numbered for both Android and iOS: "1. Open Signal on your phone. 2. Go to Settings, then Linked Devices. 3. Tap the plus button (Android) or Link New Device (iOS). 4. Scan this QR code."
- Below instructions, an informational chip in a calm tone: "Signal officially supports secondary devices. There is no ban risk. History from before this pairing is not synchronized: Twalk will see new messages only."
- Footer: "Troubleshooting" link.

**States.** Same lifecycle as screen 3a (waiting for scan, expired, scanning, success, failure) but without the mandatory disclosure card because there is no ban risk to disclose.

**Server-Sent Events.** `/api/bridges/signal/login/status`. On success, `bridge.status.changed.v1` with `network=signal`, `to_state=connected`.

**Success criteria.** User completes pairing in under 2 minutes and understands the history limitation.

---

## Screen 3c — SMS via Google Messages (v0.1 preview)

**Status.** Implemented in v0.1 as a proof of concept. Replaced by screen 3c-next in v0.2.

**Purpose.** Complete the mautrix-gmessages bridge login. This is the v0.1 SMS path: it validates the end-to-end SMS pipeline (Sensor decryption, CloudEvents publication, agent processing, outbound reply) without committing to a mobile release. The sovereignty gap is disclosed to the user upfront.

**Trigger.** User tapped the SMS card on screen 3.

**Layout.**

- Header: back arrow, title "Connect SMS via Google Messages", small "Preview" badge.
- **Preview and migration disclosure (mandatory before showing the login flow):** informational card, amber tone: "This SMS path is a v0.1 proof of concept. It relies on Google Messages Web, which means SMS transit through Google's infrastructure and a Google account is required. In v0.2, Twalk replaces this path with a first-party Android app that reads SMS directly from your phone with no third-party in the middle. Your existing SMS setup will migrate automatically when v0.2 ships. Learn more." Two buttons: "I understand, continue" and "Learn more" (opens [ADR 0004](../architecture/adr/0004-twake-sms-companion-first-party-app.md) rendered in a modal).
- Body top: informational card explaining the requirement: "Twalk uses the Google Messages app on your Android phone to send and receive SMS. You need Google Messages installed and set as your default messaging app on Android. iPhone-only users cannot use this preview."
- Body middle: two-step flow.
  - **Step 1: Enable Google Messages Web on your phone.** Instructions: "1. Open Google Messages on your Android phone. 2. Tap your profile picture, then Device Pairing. 3. Tap 'QR code scanner'. 4. Keep this screen open on your phone."
  - **Step 2: Sign in to your Google account.** Because Google [deprecated the QR code login](https://docs.mau.fi/bridges/go/gmessages/authentication.html) for third-party bridges in 2024, mautrix-gmessages now requires a Google account cookie login. The Companion opens an in-app browser view pointing to the Companion Gateway's `/api/bridges/gmessages/login/google` endpoint, which walks the user through Google's OAuth-like challenge (email, password, 2FA, cookie extraction). Explicit disclosure: "Twalk stores a Google session cookie on your Twalk server to keep the SMS bridge alive. This cookie is scoped to Google Messages Web and is stored encrypted. You can revoke it any time from your Google account, or from the Twalk dashboard."
- Body bottom: "Troubleshooting" link with common issues (Google 2FA challenges, session revoked by Google, phone offline).

**States.**

- *Disclosure shown:* login flow hidden until the user confirms the disclosure. Confirmation is stored locally in the Companion PWA.
- *iOS user detected:* screen replaced by a friendly explanation, "SMS through Twalk in v0.1 requires an Android phone. If you have both an iPhone and an Android device, you can use the Android device as your SMS source. The v0.2 sovereign SMS path will still require an Android phone as well." With a "Skip SMS for now" button.
- *Waiting for Google login:* spinner during the in-app browser step.
- *Cookie captured:* success animation, "Twalk is now sending and receiving your SMS through Google Messages. This channel will migrate automatically to the sovereign path when v0.2 ships."
- *Failure:* specific error messages for Google 2FA rejection, network error, or session already active on too many devices.

**Server-Sent Events.** `/api/bridges/gmessages/login/status`. On success, `bridge.status.changed.v1` with `network=sms`, `to_state=connected` (`bridge_id` identifying the mautrix-gmessages instance).

**Migration to v0.2.** When v0.2 ships, users on the mautrix-gmessages path see a persistent banner on their dashboard offering a one-click migration to the Twake SMS Companion. During migration, both bridges run in parallel for 30 days to avoid message loss, then mautrix-gmessages is decommissioned per user. Migration mechanics are documented in the v0.2 release notes and in a follow-up wireframe document.

**Success criteria.** Users complete SMS onboarding in v0.1 while fully understanding that this is a preview path and that a sovereign migration is planned for v0.2.

---

## Screen 3c-next — SMS via the Twake SMS Companion Android app (v0.2 target)

**Status.** Design specification for v0.2. Not implemented in v0.1. Retained here so the v0.2 build starts from an aligned specification rather than a blank sheet. See [ADR 0004](../architecture/adr/0004-twake-sms-companion-first-party-app.md) for the architectural rationale.

**Purpose.** Guide the user through installing the Twake SMS Companion on their Android phone and pairing it with their Twalk deployment. This replaces the v0.1 mautrix-gmessages path with a sovereign flow that keeps SMS on the user's phone and their Twalk server, with no third party in the middle.

**Trigger.** User tapped the SMS card on screen 3.

**Design intent.** SMS is a legally significant channel (administrative notifications, banks, doctors). Twalk's SMS path must be self-contained: the messages must transit only between the user's Android phone and the user's Twalk deployment, with no cloud intermediary. The Twake SMS Companion is a small open-source Android app that Twalk provides for that exact purpose. It uses Android's official [SmsManager](https://developer.android.com/reference/android/telephony/SmsManager) API to send SMS, and registers as a Notification Listener plus SMS BroadcastReceiver to receive them. It publishes messages to a dedicated Matrix appservice room configured on the user's Twalk homeserver.

**Layout.**

- Header: back arrow, title "Connect SMS".
- Body top: informational card, calm tone: "To send and receive SMS through Twalk, install the Twake SMS Companion on your Android phone. It reads and writes SMS on your phone using the standard Android APIs, and forwards them to your Twalk server over an end-to-end encrypted Matrix session. No Google account is involved. No third-party cloud sees your SMS."
- Body middle: three-step flow.
  - **Step 1: Install the app.** Two side-by-side buttons: "F-Droid" (recommended) and "Google Play". Below, a large QR code that opens the F-Droid listing when scanned. Below that, a smaller link for advanced users: "Direct APK download (verify signature)".
  - **Step 2: Grant SMS permissions.** Screenshot walkthrough showing the two prompts the Twake SMS Companion asks for on first launch: SMS Read and Send permission, and Notification Listener access.
  - **Step 3: Pair the app with this Twalk deployment.** The Companion PWA generates a pairing QR code by calling `/api/channels/sms/pair` on the Companion Gateway. The QR encodes a short-lived pairing token, the Twalk homeserver URL, and the appservice room id. Instructions: "1. Open Twake SMS Companion on your phone. 2. Tap 'Pair with Twalk'. 3. Scan this QR code."
- Body bottom: "Troubleshooting" link, and an explicit note: "Your Android phone must stay online for SMS to flow through Twalk. If your phone is off, incoming SMS will queue on your carrier and be delivered to the Twake SMS Companion when the phone comes back online."

**States.**

- *iOS user detected:* screen replaced by a friendly explanation, "SMS through Twalk requires an Android phone. iOS does not allow apps to read or send SMS on behalf of the user. If you have both an iPhone and an Android device, you can use the Android device as your SMS source." With a "Skip SMS for now" button that returns to screen 3.
- *Waiting for pairing:* QR visible with a countdown showing the pairing token expiry (10 minutes).
- *Pairing token expired:* QR replaced by a "Refresh" button.
- *Paired, first message received:* success animation, "Twake SMS Companion is now paired with your Twalk deployment. You are all set."
- *Failure:* explicit error messages for pairing token invalid, homeserver unreachable from the phone, or permissions denied on the phone.

**Server-Sent Events.** `/api/channels/sms/pair/status`. On success, `bridge.status.changed.v1` with `network=sms`, `to_state=connected` (`bridge_id` prefixed with `bridge-sms-` to keep the taxonomy consistent even though this is not a Mautrix bridge).

**Notes.**

- The Twake SMS Companion is planned to be developed as a first-party LINAGORA open-source project, released on F-Droid and on the Play Store. Distribution on F-Droid is the primary channel and matches Twalk's sovereign stance.
- RCS support is out of scope for v0.1. Android does not expose a public RCS API to third-party apps; RCS remains a Google-controlled channel through Google Messages. If a public RCS API becomes available, the Twake SMS Companion will add support.
- The Twake SMS Companion runs entirely on the user's phone. It does not use Firebase Cloud Messaging, it does not phone home to LINAGORA, and it does not require Google Play Services. Wake-ups are handled by Android's SMS broadcast intents (system-level events, delivered even when the app is in the background).

**Success criteria.** Android users complete pairing in under 5 minutes. iOS users get a clear message and a graceful skip. The SMS path involves no third party beyond the user's carrier.

---

## Screen 3d — Connect an existing Matrix account

**Purpose.** Allow users who already have a Matrix account (on any homeserver, not just the Twalk-provisioned one) to bring it into Twalk without creating a new identity. This is the sovereign path preferred by advanced users.

**Trigger.** User tapped the Matrix card on screen 3, or chose the "I already have a Matrix account" link on screen 1.

**Layout.**

- Header: back arrow, title "Connect your Matrix account".
- Body top: caption "If you already use Matrix, Twalk can watch over your existing account rather than creating a new one. Twalk will invite its Sensor into the rooms you allow, without changing anything to your Messagr, Element, or other Matrix client sessions."
- Body middle: three login methods, presented as tabs or a segmented control.
  - **Sign in with QR code (recommended).** Instructions: "1. Open a Matrix client that supports [MSC4108 sign-in with QR](https://github.com/matrix-org/matrix-spec-proposals/pull/4108), such as Messagr, Element X, or any other compatible client. 2. Go to Settings, then Sign in another device. 3. Scan the QR code shown below." The QR is a device grant code generated by the Companion.
  - **Sign in with password.** Fields: homeserver URL (pre-filled with the Twalk domain, editable), username, password. "Sign in" button.
  - **Sign in with SSO.** Detected automatically if the homeserver advertises it. Opens the homeserver's SSO page in an in-app browser view.
- Body bottom: informational card, calm tone: "Twalk will provision its Sensor account on your homeserver (or on the Twalk homeserver if your homeserver denies external accounts). You will be asked to verify the Sensor with cross-signing right after this step. This is what tells your other Matrix devices that the Sensor is trusted to receive encryption keys."
- Footer: "Learn more about the encryption model" (opens the security documentation, notably [ADR 0003](../architecture/adr/0003-sensor-encryption-boundary.md)).

**Post-login step (same screen, second stage).**

After successful authentication, the screen transitions to a **Sensor verification** state:

- Title changes to "Verify the Twalk Sensor".
- Body shows the Sensor's cross-signing master key emoji verification, or its recovery key fingerprint.
- Instructions: "Open your primary Matrix client, go to the Sensor device in your device list, and confirm the emoji sequence." The emojis are shown large and readable.
- Confirm button, enabled once the Companion detects the cross-signature via a Matrix `/sync` poll.

**States.**

- *Homeserver detection:* on password login, when the user types a `@handle:homeserver` in the username field, the Companion auto-detects SSO capabilities and updates the available methods.
- *2FA required:* when the homeserver has SSO with 2FA, the in-app browser handles it transparently.
- *Sensor verification pending:* clear waiting state with the emoji sequence and a "Waiting for you to confirm on your other device..." message.
- *Verification success:* transition to the persona activation screen (Screen 4).
- *Verification timeout:* after 5 minutes, offer a fallback "Verify with recovery key" which asks for the user's Matrix recovery key.

**Notes.**

- For an existing Matrix account, there is no WhatsApp/Signal/SMS bridge configured automatically. The user still goes through screens 3a, 3b, 3c on demand.
- If the user's existing homeserver refuses the appservice registration for the Sensor account (which is required by [ADR 0003](../architecture/adr/0003-sensor-encryption-boundary.md)), the Companion offers to provision the Sensor on the Twalk homeserver instead, with a clear explanation of the trust implications.

**Success criteria.** Advanced Matrix users bring their own account without friction. Emoji verification is understood and completed in under 2 minutes.

---

## Screen 4 — Persona activation: Assistant

**Purpose.** Introduce the concept of personas and activate the first one (`assistant`) with a safe default.

**Trigger.** At least one channel has been connected in screen 3. The screen adapts the introductory sentence to name the actually-connected channels (WhatsApp, Signal, SMS, Matrix rooms).

**Layout.**

- Header: back arrow, title "Activate your first agent", step indicator "2 of 3".
- Body top: card with the persona name "Assistant" and a one-sentence description "Your assistant reads your incoming WhatsApp messages and can suggest polite replies for your review. It never sends anything without your approval."
- Body middle: three toggle rows explaining what the assistant will do:
  - "Read incoming messages" — pre-toggled ON, cannot be turned off (this is the minimum requirement to be useful).
  - "Suggest replies for your review" — pre-toggled ON.
  - "Auto-send replies you have approved once" — pre-toggled OFF (opt-in for a later, more advanced use).
- Body bottom: expandable "Advanced settings" section with active hours (default: always active) and consent default (default: `pending` for unknown contacts, `granted` for contacts already in the user's phone book).
- Primary CTA: "Activate the assistant".

**States.**

- *Ready:* CTA enabled.
- *Activating:* CTA becomes a spinner, "Waking up the assistant...".
- *Success:* transition animation, "Your assistant is now watching WhatsApp." Continue to Screen 5.

**CloudEvents emitted.** On activation, the Companion Gateway emits a `consent.state.changed.v1` event with `subject.type=persona`, `subject.id=assistant`, `old_state=unset`, `new_state=granted`, `scope.networks=[whatsapp]`.

**Success criteria.** User understands what the assistant will do, is not surprised by any behavior later, and reaches Screen 5.

---

## Screen 5 — Home dashboard: Diagnostic and Messagr pairing

**Purpose.** The recurring home screen the user returns to. Shows the current health of the system and offers the option to pair the Companion with Messagr for a unified experience.

**Trigger.** User completed Screen 4, or opens the Companion again after any first-time flow.

**Layout.**

- Header: Twalk logo, user avatar (top right, opens account menu).
- Body top: system status card
  - Overall health indicator (green dot "All systems operational" or amber/red with a summary).
  - Bridges section: one row per configured bridge, with a colored dot (green connected, amber degraded, red disconnected), the network name, last message received time, and a "Manage" arrow.
  - Personas section: one row per active persona, with an active/paused toggle and last activity time.
- Body middle: recent activity feed, showing the last 10 events on the bus (received messages, agent suggestions, bridge status changes, consent changes). Each row is a chip with an icon, a short label, and a relative time.
- Body bottom: primary CTA area
  - If Messagr is not paired yet: card "Pair with Messagr" with a short explanation and a "Pair now" button that opens a QR code scanner.
  - If Messagr is paired: card "Add another channel" with buttons for Telegram, Signal, Discord, Google Messages (each opens a flow similar to Screen 3).
- Bottom navigation bar: Home, Bridges, Personas, Consent, Settings.

**States.**

- *All green:* calm layout, muted colors.
- *Bridge session expired:* amber banner at the top, "Your WhatsApp session expired. Reconnect to keep receiving messages." with a "Reconnect" button.
- *Pending consent decisions:* purple chip near the top with a count, "3 consent decisions waiting for you", opens the consent inbox.

**Server-Sent Events.** The dashboard subscribes to a merged event stream `/api/events/stream` that delivers `bridge.status.changed.v1`, `consent.state.changed.v1`, and a summary of `agent.suggest.produced.v1`. All updates render live without page refresh.

**Success criteria.** User can, in under 5 seconds, tell whether Twalk is working normally. If not, they know exactly what to do next.

---

## Design system notes

- **Colors:** Twake teal (`#00A896`) for primary actions, amber (`#F4A261`) for warnings, red (`#E63946`) for errors, green (`#2A9D8F`) for success, gray scale for neutrals. Dark mode uses inverted contrast with the same accent colors.
- **Typography:** Inter for UI (weight 400 for body, 500 for labels, 700 for headings), JetBrains Mono for the recovery key and any technical strings. Base size 16 px, line height 1.5.
- **Spacing:** 8 px baseline grid. Component paddings are multiples of 8.
- **Motion:** transitions of 200 ms ease-out for state changes, 400 ms for screen transitions. Reduced motion mode respected via `prefers-reduced-motion` media query.
- **Icons:** Lucide icon set, 24 px default, 20 px in dense contexts.

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

v0.1 ships in French and English. Screen copy uses ICU MessageFormat placeholders for all interpolated values (username, domain, counts). Translation files live in `companion/src/lib/i18n/<locale>.json`. Default locale is inferred from `navigator.language`.

---

## Out of scope for v0.1

- Sovereign SMS onboarding via the Twake SMS Companion Android app (design retained in screen 3c-next above, implementation in v0.2, see [ADR 0004](../architecture/adr/0004-twake-sms-companion-first-party-app.md)).
- Telegram and Discord onboarding screens (deferred to v0.2).
- Multi-persona management screens (deferred to v0.2).
- Consent inbox with searchable contact list (deferred to v0.2).
- Consent policies with time windows and rule builder (deferred to v1.0).
- Bridge diagnostic deep-dive with logs and metrics (deferred to v1.0, may live in Buzz instead).
- Onboarding tour or product tour (deferred, decided only after v0.1 user feedback).
- Native mobile shell (deferred, likely never — see ADR 0002).
