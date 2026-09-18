# companion

The Twalk Companion PWA: the user-facing configuration surface, from Matrix
account bootstrap to bridge login, persona activation and consent management.
"The Companion" means this app alone — the backend is the **Companion Gateway**
(`companion-gateway/`), always named in full.

A SvelteKit static export, served by the Companion Gateway on its own origin.
It is a client of that Gateway's HTTP API and of the user's homeserver, and
nothing else. The screens it implements are designed in
`docs/wireframes/companion-v0.1.md`; the cryptographic decisions are
[ADR 0014](../docs/architecture/adr/0014-companion-crypto-runs-in-the-browser.md).

## Commands

Node 22 and npm 10. Everything runs from this directory.

```bash
npm ci               # install; also runs `svelte-kit sync`
npm run dev          # the Vite dev server, for working on a screen
npm run build        # the static export, into build/
npm run api:generate # regenerate the Gateway client from the OpenAPI description
npm run api:check    # fail if that generated client is stale
npm run check        # svelte-check (types, templates, a11y warnings)
npm run test:unit    # Vitest, over the pure modules
npm run test:e2e     # Playwright, against build/ served like the Gateway
npm test             # api:check + check + unit + build + e2e — the full suite
```

`npm run test:e2e` needs a browser once: `npx playwright install chromium`.

Nothing built is committed. `build/` is produced by the Gateway's image
(`deploy/docker-compose/companion-gateway.Dockerfile`), whose Node stage runs
`npm ci && npm run build`, and by `npm test` locally.

## How it is put together

| Path | What lives there |
|---|---|
| `src/routes/` | The screens. `+layout.ts` holds the page options that make this a static SPA; `+layout.svelte` is the shell and the capability gate. |
| `src/lib/api/` | The Gateway client. `schema.d.ts` and `gateway-version.ts` are **generated**; `client.ts` is the two lines of configuration around them. |
| `src/lib/crypto/` | The cryptographic bootstrap (ADR 0014) and the crypto store's presence. The only place matrix-js-sdk is imported, and it is imported dynamically. |
| `src/lib/recovery/` | The recovery key: its Matrix encoding, the printable PDF, the clipboard and the download. |
| `src/lib/matrix/` | Matrix's own homeserver discovery, for the domain screen 1 asks for. |
| `src/lib/onboarding/` | Screen 1 and screen 2's non-visual half: the domain, the deployment probe, the registration relay's refusals, the form rules. |
| `src/lib/session/` | Signing this browser in to the Gateway with a Matrix OpenID token (ADR 0011). |
| `src/lib/tabs/` | The Web Lock that elects one tab. |
| `src/lib/capabilities/` | The capability gate: `report.ts` decides (pure), `probe.ts` measures (browser-only). |
| `src/lib/networks/` | Screen 3 and the network flows: the card catalogue, the polled login read as a screen state (`login-view.ts`, pure), the polling itself (`login-session.ts`), each QR screen's words (`copy.ts`) and the SMS path's cookie parsing (`cookies.ts`). |
| `src/lib/matrix/` | The user's own homeserver: discovery, sign-in (`login.ts`) and the room listing screen 3d selects from (`rooms.ts`). |
| `src/lib/personas/` | Screen 4: the `assistant` card and its two locked abilities (`catalogue.ts`), the perimeter an activation is scoped to (`scope.ts`, pure) and the consent decision it writes (`activation.ts`). |
| `src/lib/dashboard/` | Screen 5's judgements (`model.ts`, pure: the rows, the health roll-up, the feed and what may not be in it), the four reads it needs (`load.ts`) and its relative times (`format.ts`). |
| `src/lib/qr/` | The QR encoder. The only module that imports an encoding library. |
| `src/lib/version/` | The version handshake against the Gateway's `/health`, and the reload it forces. |
| `src/lib/i18n/` | French and English, ICU patterns, `<locale>.json` per the wireframes. |
| `src/lib/icons/` | The one module that imports an icon library. |
| `src/lib/styles/` | `tokens.css` (the design tokens) and `base.css` (element defaults). |
| `src/service-worker.ts` | Installability. Caches the fingerprinted build and nothing else. |
| `tests/serve-like-gateway.mjs` | The Gateway's own path resolution, in Node, for Playwright. |
| `tests/real-stack.mjs` | Brings up the compose stack and the Gateway binaries the stack-backed journeys run against. |
| `tests/stub-bridge.mjs` | bridgev2's provisioning contract, stubbed — including its blocking step. |
| `tests/e2e/dashboard/bus.ts` | Forty lines of the NATS wire protocol: a journey subscribes, to assert that a consent decision reached the bus and not only the screen, and publishes, to make a contact pending without running a Sensor. |

## The decisions worth knowing before you change something

### The static export, and the contract with the Gateway

`adapter-static` with `fallback: '200.html'` and `precompress: true`
(`vite.config.ts`), `ssr = false`, `prerender = true` and
**`trailingSlash = 'never'`** (`src/routes/+layout.ts`).

`'never'` is the chosen half of the contract. It makes the adapter write a
prerendered page as `<path>.html` (`diagnostics.html`, not
`diagnostics/index.html`), which is step 2 of the resolution order in
`companion-gateway/src/static_files.rs`; the Gateway's step 3 redirects the
slashed spelling onto it, so both spellings work and the build has one. A route
the build has no file for is answered with `200.html` at HTTP **200**, so a
deep link reloaded cold loads the app rather than a 404.

### The cryptographic bootstrap, and the order it runs in

`src/lib/crypto/bootstrap.ts` is the only module that imports matrix-js-sdk, and
it imports it **dynamically**. That one line is what keeps the 1.3 MB of brotli
WebAssembly off screen 1, the capability gate and `/diagnostics`.

The order inside it is load-bearing, and ADR 0014 says why:

1. `initRustCrypto()` — the Rust crypto machine, over IndexedDB;
2. `bootstrapCrossSigning({ authUploadDeviceSigningKeys })`;
3. `createRecoveryKeyFromPassphrase()` — **with no argument**: the passphrase
   path is 500 000 PBKDF2 iterations, seconds of frozen phone for a weaker
   secret, and the wireframes offer no passphrase;
4. `bootstrapSecretStorage({ createSecretStorageKey, setupNewKeyBackup: true })`.

Swap 2 and 4 and everything still *succeeds*: secret storage exists, the backup
exists, and the cross-signing private keys are simply not in it — a recovery key
that recovers nothing, undetectable later.

The private key reaches the screen, the clipboard and a PDF built in the page,
and no request. `tests/e2e/bootstrap.spec.ts` asserts that over the whole
journey, against the 48 characters, their compact spelling, and the 32 bytes in
hex and base64.

The `.wasm` must be served as exactly `application/wasm`, with no parameters, or
`WebAssembly.instantiateStreaming` refuses it with no fallback. The Gateway does
that, `tests/serve-like-gateway.mjs` does that, and `tests/e2e/serving.spec.ts`
asserts it. `vite.config.ts` also strips the module's `name` section at build
time (7.8 MB → 4.8 MB raw, 1.29 MB brotli), which is ADR 0014's "debug symbols
stripped, brotli served" with no toolchain to install.

### Store loss is a journey, not an error

The crypto store is `matrix-js-sdk::matrix-sdk-crypto` in IndexedDB,
unencrypted (ADR 0014: any key protecting it would live in the same browser
storage). Losing it is **normal** — iOS deletes a Safari tab's IndexedDB after
seven days without interaction unless the Companion is installed to the home
screen — so `/recover` is a designed screen with its own copy, and screen 1
recognises the situation before the user asks: a live Gateway session with no
crypto store goes straight there.

What `/recover` does is a *new device joining an existing identity*: log in,
open secret storage with the recovery key, import the cross-signing secrets,
sign this device, reload the key-backup key. Nothing is reset. Resetting would
break every other device and lose the message history, which is exactly what
screen 2 warned would happen if the key were lost — and there is no reset flow
in v0.1, which screen 2 says in as many words.

One caveat worth knowing before changing it: after `crossSignDevice` the crypto
machine's *stored* copy of this device does not yet carry the signature — that
only arrives with a `/keys/query` response fed back into the machine, which
happens on a sync, and the Companion runs no sync loop. So the screen reports
what is true here and now (the identity is back and trusted, the signature was
published), and the Playwright journey asks the *homeserver* whether the device
is cross-signed.

### One tab at a time

matrix-js-sdk is explicit: two `MatrixClient` instances on one IndexedDB "will
cause data corruption and decryption failures", and the damage is silent. So
`src/lib/tabs/lock.ts` elects one tab with an exclusive Web Lock — released by
the browser when that tab dies, which no `localStorage` mutex manages — and
every other tab gets the "open in another tab" screen with a way to take over
(a `BroadcastChannel` message; the holder lets go of its client *and* its lock).
`/diagnostics` is exempt: it is where a user is sent when something is wrong.

### The Gateway client is generated, and checked

`companion-gateway/openapi.yaml` is the source of truth (ticket #63): the
Gateway embeds and serves those bytes at `/openapi.yaml`, and its own test
suite fails on a route the description does not cover.
`scripts/generate-api-client.mjs` turns it into `src/lib/api/schema.d.ts` and
`src/lib/api/gateway-version.ts`. Both are committed, because the image's Node
stage builds `companion/` alone and has no sibling directory to generate from —
and committed generated code drifts, so `npm run api:check` regenerates in
memory and fails when the two differ. It runs first in `npm test`.

**Never hand-edit those two files, and never hand-write a request.**
`openapi-fetch` types every call off the schema, so `gateway.GET('/api/devices')`
is checked against the description and a path or a response member nobody
described is a compile error.

### The session refreshes itself, and a `401` is repaired once

The device token lives fifteen minutes (`GATEWAY_DEVICE_TOKEN_TTL`, ADR 0011).
That is correct and is not to be raised: it is the credential that travels on
every request. What was missing until ticket #111 is the other half of the
mechanism — the Companion never called `POST /api/session/refresh`, so every
session died a quarter of an hour after sign-in, on whatever screen the user
happened to be reading, and the refusal rendered as a spinner that never ended.

Two mechanisms now, and both are needed:

- **the scheduled refresh** (`src/lib/session/refresh.ts`). `expires_in` comes
  back with every sign-in and every refresh, so the client renews with a fifth
  of the lifetime still in hand. A user working continuously never meets a
  failure at all. The boot sequence adopts whatever session the browser already
  holds by refreshing once — which is also the **only** way a browser can learn
  its token's lifetime, since the token is an `HttpOnly` cookie and the session
  document carries no expiry.
- **the central `401` retry** (`src/lib/api/client.ts`). A timer is a promise a
  browser does not keep: a backgrounded tab, a laptop closed over lunch, a
  phone that slept. Every `401` from every call is therefore answered once, in
  the wrapper around the generated client — refresh, replay the original
  request, and only if the refresh is refused let the failure through with the
  session marked *expired*.

One refresh at a time, always: it rotates both tokens and kills the previous
refresh token immediately, so two racing would destroy each other's
credential — and the dashboard fires five reads in one `Promise.all`, which
makes five simultaneous `401`s the ordinary case.

**No screen implements any of this**, and none should. What a screen sees is
either the answer or a failure that is genuinely terminal. When neither the
device token nor the refresh token can be saved, `SessionExpired.svelte` is
rendered by the root layout as a dialog **over** the current screen — which is
not unmounted — and its way out is `/signin?next=<the current path>` (ticket
#112), so the sign-in screen brings the user back to where they were. Sending
them to the first screen instead is how an expired session came to look like a
broken bridge, and how the owner nearly re-paired a working WhatsApp link.

### "Could not be reached" and "answered, and refused" are different sentences

`$lib/api/trouble.ts` is three words long and exists because conflating those
two cost a day. `GET /api/bridges` answered `401` in zero milliseconds — the
session had expired — and screen 3 told the owner their Twalk server could not
be reached. It had been reached. It had refused. The user goes and looks at
their firewall for a problem that is a sign-in.

So a failed call is read as one of three things — nothing answered
(`unreachable`), it answered `401` and the wrapper's refresh could not save the
session (`session-refused`), or it answered something else (`refused`) — and
every screen that reports a failed read says which. The retry button is offered
for the two a retry can help; the third offers signing in.

### Browser-only code stays out of module scope

Prerendering imports every statically reachable module in **Node**, where
`indexedDB`, `window` and `crypto.subtle` do not exist. A reference to one at
module scope breaks `npm run build`, not just the page. So:

- `src/lib/capabilities/probe.ts` is imported with `await import(...)`, from
  `src/lib/boot.ts`;
- `src/lib/version/reload.ts` likewise;
- everything else touches a browser API inside a function, called from
  `onMount` or an event handler.

`vitest.config.ts` deliberately runs **without** the SvelteKit plugin, so a
module that broke this rule would not even load in the unit tests.

### The capability gate names a cause, not a list

Secure context, WebAssembly, IndexedDB and Web Crypto are required; service
workers and Web Locks are not (missing them costs installation and the two-tab
warning). But the useful output is the *cause*: iOS Lockdown Mode switches off
IndexedDB, service workers and Web Locks while leaving WebAssembly running, and
"IndexedDB is missing" is true there and helps nobody.
`src/lib/capabilities/report.ts` distinguishes an insecure origin, Lockdown
Mode, blocked site data and an unsupported browser, and the screen leads with
whichever it found.

### No telemetry

No analytics, no error collector, no third-party request of any kind — one
Playwright test asserts that every request the app makes goes to its own
origin, which is why the fonts are self-hosted (`@fontsource-variable/*`)
rather than loaded from a CDN. What the user gets instead is `/diagnostics`
and a "copy diagnostics" button. `src/lib/diagnostics.ts` builds that text, and
its unit test asserts both halves: that it carries what a maintainer needs, and
that it carries nothing about the owner or their conversations.

### `events` is a direct dependency on purpose

matrix-js-sdk imports the Node `events` builtin without declaring it. Today it
resolves only by accident, through matrix-widget-api, which is a transitive
dependency nobody promised to keep. Declaring it here is what stops the crypto
work of ticket #67 from breaking on a dependency bump. Nothing in this app
imports it directly yet.

### Activating a persona is a consent decision, and the screen offers nothing else

ADR 0013: there is no persona control API, and there will not be one. Activation
is `POST /api/consent/decisions` with a `persona` subject, scoped to the
networks that persona may read; pausing is the same call with `revoked`. So
screen 4 may only offer what is expressible as that one decision — which is why
its two ability rows are drawn on and locked rather than switchable: a decision
carries a subject, a state and a scope, and nothing else, so a third switch
would record nothing. The design review (#74) struck the wireframe's auto-send
toggle and its active-hours control for the same reason from the other
direction: a control in a privacy screen that the runtime does not enforce is a
protection the user is told about and does not have.

The scope is fixed at the moment the user presses the button, from the bridges
whose login is `complete`. It never widens: a network connected next week is in
no decision the user took, and the persona stays inactive on it. Matrix is the
one network the Companion cannot prove is connected — the Gateway forgets the
access token that invited the Sensor as soon as the call returns (ADR 0011), and
there is no `GET /api/bootstrap/rooms` — so its row is offered unticked with the
reason on the screen rather than guessed at.

### The dashboard's feed is operational only, and that is enforced in the model

The wireframe's screen 5 fed "the last 10 events on the bus (received
messages…)" with who sent them. The design review replaced it with operational
events — bridge state, consent decisions, persona activity — plus a message
**count**. A home screen is unlocked in public; the list of who writes to you is
not something a product whose argument is sovereignty renders there.

The rule lives in `src/lib/dashboard/model.ts`, not in the markup, because a
consent decision *does* carry the contact's Matrix ID and that is the one place
this screen could leak it. `activityFeed` renders such a decision as "a
contact's consent on WhatsApp became granted" and puts no id in the row's
values; `model.test.ts` asserts it, and the Playwright journey greps the whole
rendered page for a Matrix ID.

### The pending chip is a count, and the list behind it never arrives

`GET /api/contacts/pending` (#54) answers with `total`, a per-network
breakdown, **and** `contacts` — the Matrix IDs of everyone who has written and
not been decided about. That array is the most sensitive document this origin
serves, and it is dropped in `$lib/dashboard/load.ts`, at the seam, rather than
carried into a component where a later row could render it. The chip is
therefore a number by construction and not by discipline.

It leads nowhere, and says so: the searchable consent inbox is v0.2, and the
copy points at the one decision v0.1 does offer — granting or revoking a whole
network, which decides for everyone on it at once. A chip linking to a screen
that does not exist would be worse than one that explains itself.

`null` (the deployment projects no inbound stream, or the read failed) and `0`
(nobody is waiting) are different facts and both draw nothing.

### The dashboard says what it cannot know

Three facts the wireframe assumes have no source in the deployment as it stands,
and the screen states their absence rather than drawing a plausible value: live
bridge state and the time of the last message (`GET /api/bridges` reports the
last login *process*, not a heartbeat; #56 landed the producer — bridges push
their state, the Gateway publishes `bridge.status.changed.v1` — and no read on
this origin serves it to a browser), a message count (nothing between the bus and the
Companion counts), and persona output (Hermes is not implemented — only its test
harness landed, so an activated assistant produces nothing today).

There is also no `GET /api/events/stream`. The wireframe subscribes to a merged
SSE stream; the Gateway describes no such endpoint, so the screen polls and
offers an explicit refresh. That stream now has something live to carry, and is
still the missing half.

## Testing

**Playwright is authoritative** (spec #65). It runs `channel: 'chromium'` — the
real browser in its new headless mode — against `build/`, served by
`tests/serve-like-gateway.mjs`. That server is not a convenience: it
transcribes the Gateway's resolution order, content-type table and
pre-compressed-sibling handling, because a test server that resolves paths
differently proves a routing contract nobody ships. `localhost` is a secure
context, so no flags and no HTTPS are needed for `crypto.subtle`, IndexedDB or
a service worker.

Vitest covers the pure modules only: the capability report, the version
comparison, locale negotiation, hostname validation, the diagnostics text.
Anything touching crypto, the session or the Gateway goes through Playwright.

**The bootstrap journey runs against the real stack.** `npm run test:e2e:stack`
sets `TWALK_TEST_REAL_STACK=1`, and `tests/real-stack.mjs` then brings up the
shared Docker test stack (`tests/harness/compose.test.yaml`), builds and starts
the real Companion Gateway binary against that Synapse, and
`tests/serve-like-gateway.mjs` proxies `/api` to it — same origin, so the
`HttpOnly` device cookie behaves exactly as it does in a deployment. It needs
Docker and a Rust toolchain, so `npm test` leaves it off and
`tests/e2e/bootstrap.spec.ts` skips itself with a reason rather than passing
quietly.

Each run invents its own owner localpart and gives the Gateway an empty state
directory, because the Gateway creates a deployment's **one** account and
refuses a second — that way the journey is repeatable without tearing down a
stack the Rust suites share. `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT` and
`TWALK_TEST_NATS_PORT` move the stack aside for a parallel worktree, and
`TWALK_TEST_PORT` moves the origin — worth setting both when another worktree
is running its own suite, since `reuseExistingServer` will otherwise happily
reuse *its* server.

iOS behaviour — Safari's seven-day eviction, the installed-app exemption,
Lockdown Mode — is verified by hand on a real device, as spec #65 requires:
Playwright's WebKit is not Safari.

### The QR code is drawn here, and the redraw follows `generation`

bridgev2's QR step is `{"type": "qr", "data": "…"}`: **the bridge renders no
image**, and the Gateway passes the raw payload through
(`BridgeLoginStep.payload` in `companion-gateway/openapi.yaml`). So `src/lib/qr/`
encodes it — error correction L, which is what WhatsApp's and Signal's own
clients use and what keeps the module count low enough for a phone camera — and
draws it as one SVG path with the four-module quiet zone the specification
requires.

The browser **polls**; it never holds a request open. The Gateway holds the
bridge's blocking step itself (ticket #55) and `GET /api/bridges/{id}/login`
answers immediately, so a phone that sleeps mid-scan loses a poll rather than
the login.

What says a fresh code arrived is `generation`, not the clock.
`step.valid_for_seconds` is documented as the Gateway's own *estimate* of the
network's refresh interval — WhatsApp's whole budget is about 2m40 across
refreshes — so the countdown on screen is the wireframe's decoration and the
`{#key}` the code is drawn under is the generation.

### The networks screens store nothing

A QR payload and a Google cookie are network credentials in flight. They live in
a component's state and in the SVG on screen, and are gone when the next
generation replaces them; nothing on these paths writes to `localStorage` or
IndexedDB. The one exception is a boolean: whether the user has dismissed a
screen's disclosure card, which the wireframe asks to remember per device.

### Three origins under `npm run test:e2e:stack`, because one Gateway cannot be all three

`tests/real-stack.mjs` brings up one compose stack and builds the Gateway once,
then starts it **three times**:

- `startRealStack()` — the bootstrap journey's Gateway, whose owner's account
  does *not* exist, because creating it is what the journey does and the
  Gateway creates this deployment's one account and refuses a second;
- `startBridgeStack()` — the networks journey's Gateway, whose owner is
  `bot_alpha` (already provisioned on the shared test stack), with the three
  bridges of `tests/stub-bridge.mjs` configured;
- `startSessionStack()` — the session journeys' Gateway (#111), the same owner
  and the same stub bridges, on a deployment whose **device token lives five
  seconds**. The ticket asks for the expiry to be arranged at the Gateway
  rather than waited for, and `GATEWAY_DEVICE_TOKEN_TTL` is the operator's own
  knob for it. It cannot be the bridge origin's, because that lifetime is
  deployment-wide and every other spec there would spend its life mid-expiry.

One Gateway cannot be all three, which is the whole reason for the extra
origins. Everything else is shared: one orchestrator, one `proxyToGateway` in
`serve-like-gateway.mjs`, one `TWALK_TEST_REAL_STACK=1` gate. The bridge and
session origins also expose the stub's control surface at `/stub-control/*`, so
a browser journey drives both sides of a login — the user's and the bridge's —
without knowing a second port.

The bridge is stubbed for the reason spec #47 gives: a real mautrix-whatsapp
needs a live WhatsApp account and a human with a phone. Nothing else is stubbed.

### Screen 3d learns the rooms, and the Gateway learns the selection

The room list is read **here**, from the user's own homeserver with the user's
own session, and never sent anywhere. What crosses to `POST /api/bootstrap/rooms`
is the ids the user ticked and a Matrix access token the Gateway uses for that
one call and forgets (ADR 0011) — an invitation needs the inviter's own session,
which is why the token is a parameter at all.

Two things that bite:

- A room's *name* is unencrypted state, so it reads fine without the crypto
  stack. What does not is the name of a room that has none — a direct message,
  which every client computes from member display names that `lazy_load_members`
  deliberately does not fetch. So `roomLabel` returns the name, else the alias,
  else the heroes, else the id, and **says which**, rather than rendering blank.
- Synapse puts a freshly created room's `m.room.name` in the `/sync` **timeline**
  and leaves `state` empty, because the room's whole history fits. Reading only
  `state` lists every new room as nameless. Both are read, newest wins.

Sign-in covers SSO as well as a password, and offers one button per advertised
identity provider, labelled with the provider's own name and redirecting to
`/sso/redirect/{idpId}`: a homeserver with SSO usually has *only* SSO, and a
password form there is a dead end. The `loginToken` the homeserver redirects
back with is a short-lived credential, so it is spent and stripped from the
address bar at once — a copied or bookmarked URL must not carry one.

### Screen 3c asks the user to copy cookies, and says why

Google switched off the QR sign-in for third-party Google Messages clients in
2024, so mautrix-gmessages signs in with the user's Google session cookies. The
wireframe imagined an in-app browser doing the extraction; a PWA has none, and
no page may read another origin's cookies — that is the same-origin policy, not
a gap to route around. So the user copies them, and the screen's job is to be
honest about what that means: what the cookies are, that Google documents no
lifetime for them so Twalk promises none, that a **private window** is required
(signing out of a normal one invalidates the session the bridge holds) and that
Chrome's **Device Bound Session Credentials** must be off (it binds the session
to this device's hardware key, so a copied cookie works nowhere else).

`cookies.ts` accepts the three spellings a user actually arrives with — a
`Cookie:` header, a JSON object, a cookie-extension export — and names the
cookies that are missing rather than saying "invalid". The paste is cleared
before the request goes out and is written to no browser store.

## What is not here yet

Screen 1's secondary "pair with my other device" link waits on the
device-pairing flow and is not wired. MSC4108 sign-in by QR from another Matrix
client is named on screen 3d rather than offered: it carries encryption secrets
across and needs the crypto stack these screens do not load. Messagr pairing is
shown on the dashboard as unavailable in v0.1, which it is.
