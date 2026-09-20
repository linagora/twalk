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
| `src/lib/networks/` | Screen 3 and the network flows: the card catalogue, the polled login read as a screen state (`login-view.ts`, pure), the polling itself (`login-session.ts`), what a step's fields become on screen and what the answer to them looks like on the wire (`step-fields.ts`, pure), this project's own words for a bridge's step and the drift that makes them stale (`step-copy.ts`, pure), the seam between the flow and the panels (`login-panels.ts`), each login screen's words (`copy.ts`) and the cookie parsing a jar is read with (`cookies.ts`). |
| `src/lib/matrix/` | The user's own homeserver: discovery, sign-in (`login.ts`) and the room listing screen 3d selects from (`rooms.ts`). |
| `src/lib/personas/` | Screen 4: the `assistant` card and its two locked abilities (`catalogue.ts`), the perimeter an activation is scoped to (`scope.ts`, pure) and the consent decision it writes (`activation.ts`). |
| `src/lib/dashboard/` | Screen 5's judgements (`model.ts`, pure: the rows, the health roll-up, the feed and what may not be in it), the four reads it needs (`load.ts`) and its relative times (`format.ts`). |
| `src/lib/portals/` | Which conversations a network is observed on (#143): the kind each one is, from the network's own identifier (`conversations.ts`, pure); what a tick costs in people, and when that has to be acknowledged (`selection.ts`, pure); and the two calls that read and write it (`register.ts`). |
| `src/lib/components/login/` | One bridge login: `BridgeLogin.svelte` owns the flow, one panel per kind of step draws it, and `panels.ts` is the typed table that makes a kind nobody draws a compile error (ADR 0030). |
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
| `tests/e2e/portals/` | The conversation chooser's journey (#143): real portal rooms built through the stack's appservice, a real Sensor beside the Gateway, and the bus read at the end of it. |

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

The take-over has a terminal state, and the reason is worth keeping (#135). It
used to spin for ever when nothing answered, and the obvious diagnosis — "the
holder is gone" — is the wrong one: a Web Lock dies with its tab, so a lock
still held is a tab still alive. It is a tab the browser has **frozen**, which
keeps the document and its lock while running none of its JavaScript. So
`takeOver` answers within five seconds either way, and the screen names the
remedy that works on a frozen tab: find it and close it (switching to it wakes
it), or restart the browser.

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

### An action's answer is rendered where the action is

A refusal that nobody reads is not better than no refusal. The owner ticked a
room out of about a hundred, scrolled to the bottom, pressed the invite button,
and reported that nothing happened: the Gateway had refused, the screen had set
its message, and the message was three thousand pixels above the button (#139).
The code was right, the string was right, and the user was told nothing.

So `$lib/components/ActionProblem.svelte` is the one decision, applied to every
screen whose action can be scrolled away from its message: the alert is rendered
beside the control that caused it, carries `role="alert"` so it is announced
wherever it is, and scrolls itself into view with `block: 'nearest'` — which
moves nothing when it is already visible. A screen with two actions a page apart
has two message surfaces and keeps them apart; screen 3d is the example.

Note what could not catch this: four hundred tests that click by selector. The
test that does asserts `toBeInViewport` after acting at the bottom of a hundred
rooms.

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
`$lib/networks/connection.ts` says are connected — the bridge's own answer, and
never `ConfiguredBridge.login`, which is a login *process* the Gateway holds in
memory and forgets. Reading the latter left this screen offering an empty
perimeter on a deployment with two networks connected, so no persona could be
activated at all (#142). It never widens: a network connected next week is in
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

Since #170 it leads to `/consent`, which is where that array is read and acted
on. The reduction stays exactly where it was, for the reason the approval chip
gives: a home screen is what gets unlocked on a train, so this one carries a
number and a link and nothing else.

`null` (the deployment projects no inbound stream, or the read failed) and `0`
(nobody is waiting) are different facts and both draw nothing.

### The consent screen shows three states, because the model has three (#170)

`/consent` and `$lib/consent/` are the screen the product's central promise had
no interface for. Four decisions in it are the ones to argue with.

**`pending` is not `revoked`, and never-decided is neither.** `$lib/consent/model.ts`
carries `state` (`granted`, `pending`, `revoked`) and `decidedBy` (`contact`,
`network`, `nothing`) as two facts, because ADR 0010 is explicit that an absent
subject means no decision was ever recorded and never a revoked one. `awaiting`
is `decidedBy === 'nothing'` and is deliberately *not* a synonym for
`state === 'pending'` — the Gateway draws the same line, since
`GET /api/contacts/pending` lists exactly the rows whose `decided_by` is `null`
and a contact the owner deliberately left `pending` is not in it. Most of a real
list is never-decided (eighteen conversations appeared on the reference
deployment in one day), so a screen with two states where the model has three
teaches the user a wrong picture of their own deployment.

**Only `revoked` withholds content, and the legend says so.** `granted` lets a
persona read; `pending` publishes the message in full and consumers refuse it by
convention; only `revoked` reduces what is published (ADR 0012). `withholds` is
a function in the model rather than an adjective in a catalogue, because copy
implying that undecided means unseen would be a comforting untrue thing.

**None of it is retroactive, said twice.** The label is stamped by the Sensor at
publication, so a decision taken now reaches nothing already on the bus. It is
in the legend and again against the decision the user has just taken, because a
user who grants a contact and sees nothing happen would otherwise conclude the
product is broken.

**The bulk control is scoped to the filter, and asks twice.** #137's rule with
higher stakes: `bulkDecisions(shown, state)` takes the rows on screen and
nothing else, the number on the button is that array's length, and there is no
function in the module that takes the whole list. Two presses, because "grant
all" over a list containing a 246-member association is the affordance this
screen exists to avoid. Answering for a whole *network* is not offered here at
all — it decides for people who have not written yet, and #122 is the open
question about how it should be.

The owner is **labelled, not filtered**. ADR 0018 and ADR 0021 say the owner has
no consent state on any event; a row about them is #149's Gateway half showing
through, and hiding it would hide the only symptom a user can see. What this
browser can recognise is the owner's canonical Matrix ID, from the session — the
network ghosts #149 is actually about cannot be recognised here, because the
Gateway does not know them either.

The journey (`tests/e2e/consent/`) is the only Companion suite that starts a
**real Sensor**, and `tests/e2e/consent/sensor.ts` says why: the consent label is
stamped by the Sensor, so a test that published the label itself would assert its
own string. That Sensor runs with **no Gateway snapshot configured**, so its cache
starts cold and everything is `pending`; a `granted` label can only have come
from the decision the browser took, through the Gateway's outbox, onto the bus,
into the Sensor's cache. It is also the one shape of this journey no other suite
covers — `sensor/tests/consent.rs` proves relabelling on WhatsApp from a decision
the test published, and this proves it on `matrix` from a decision a user took in
a browser.

### The approval screen cannot name the contact, and says so (#160)

`/approvals` draws what a persona proposed and sends the one the user approves.
The hard part is not the sending; it is what the screen is allowed to know.

`GET /api/suggestions` (#97) carries the message a suggestion answers as a
CloudEvents id and a type and **nothing else** — no sender, no display name, no
excerpt. That is not an oversight but the strongest form of the protection #110
established: the Gateway's projection has no code path that opens an inbound
event, so there is no reduction rule to get right and no leak to test for. The
cost lands here. The screen can say *"a reply to a message on WhatsApp"*; it
cannot say *"a reply to Aïcha"*.

This screen does not work around it. There is no second read, no cache and no
member on a row where a contact could go, and `rows.test.ts` asserts that a row
serialises without one. What it does instead is say so on the screen, beside the
row it affects (`approvals.whoIsIt.*`), so that a user asked to be deliberate
about something they cannot see knows it is a decision rather than a defect.

**What building it showed**, for #160 to weigh: the reply's own text carries
more context than expected — a persona answers in the language of the message it
answers and usually quotes its subject, so a single suggestion reads perfectly
well on its own. What genuinely hurts is *two suggestions at once*: two cards,
both plausible, and nothing on either to say which conversation it belongs to.
The failure is not "I cannot tell who this is", it is "I cannot tell these two
apart", and it arrives the moment a deployment has more than one active
correspondent. Of #160's four options, the one this screen would have used is a
`contact` member on the suggestion event itself — written once, at publication
time, where ADR 0012 already puts these decisions.

### Refusing a suggestion is local, and the copy says exactly that

#100 asks for three actions per row. Two of them are `POST /api/approvals`. The
third — **refuse** — is not any API: this origin serves no route that refuses a
suggestion, and a suggestion lives in the stream rather than in a store, so a
refusal recorded at the Gateway would be the second store `CONTEXT.md` refuses
to have, and a refusal published on the bus would be a new contract event type
before the v1.0 freeze whose only consumer is this screen.

So refusing hides the row **on this browser** (`$lib/approvals/dismissed.ts`),
the persona is not told, nothing is recorded, the suggestion goes stale on its
own — and all four of those are on the screen next to the button. The dismissal
can be taken back, and the journey asserts that the Gateway still calls the
suggestion `approvable` afterwards, because a local hiding that pretended to be
more would be the screen lying about what it did.

### Every refusal has a sentence, and a test reads the Gateway to prove it

`POST /api/approvals` refuses with fifteen distinct codes plus the three the
request itself can be wrong in, and the Gateway's own description says why they
are not one code. `$lib/approvals/refusal.ts` is the table of sentences, and
`refusal.test.ts` parses `companion-gateway/openapi.yaml` and fails when the
approval or suggestion routes grow a code this screen has no words for — in both
directions, so a removed code leaves no orphan string either.

Two invariants the type enforces rather than the screen: every answer names a
**remedy**, so there is no cause without a next step; and there is no value
meaning "still working", so a refusal cannot be rendered as a spinner (#111,
#135, #139). The one refusal that is not a failure is
`approval_published_but_not_recorded` — the reply *went out* and the Gateway
could not write that down — and it carries `sent: true`, because telling the user
it failed is the lie that makes them send twice.

### Lost replies, in the one form this origin can see

A suggestion whose `standing` is `approved` and whose approval's `publication`
is `unpublished` is a reply this deployment recorded and never sent. It is listed
with its text and a *Send it again* action, which republishes under the same
deterministic id and is deduplicated by the bus, so it cannot go out twice. Both
values of `publication` are terminal and neither is "in flight".

The other kind — a reply that reached the bus and that the Sensor could not post
into the room — is **not visible from this origin at all**: nothing publishes an
event for it and no route reports it. The screen does not invent a row for it.

### Published is not delivered, and the screen never says "sent" (#216)

The first human approval on the reference deployment was true in every word on
the screen — approved by the owner, published at position 2581, posted into the
room — and the contact received nothing: the owner's account was not in the
portal room, and a bridge relays only what the logged-in user's own account
sends. So the screen has two sentences where it had one, and neither is "sent".

**Before the button**, `delivery` — the Gateway's read of where the owner's own
account stands in the trigger's room, asked of the homeserver as the bridge's
bot. `cannot_reach` is a certainty and is drawn as a warning: approving would
publish a reply nobody receives, and the copy names #123 (the device that acts
in the owner's name has to have joined the conversation) so a reader knows it is
a known gap in the mechanism and not a fault in their setup. `can_reach` is the
register's best reading. `unknown` is a room no bridge bot can read, with its
reason, and the honest word for it — never `can_reach` on a guess. The button
stays whatever the answer: the Gateway is the authority on refusing an approval,
and a screen that hid it would be a second, disagreeing one.

**After it**, `posted` — the Sensor's own report of what the reply reached once
it posted it (`contact` or `nobody`, and by which account), or "not yet" while
there is none. `postedCopy` in `rows.ts` is what decides the sentence and it
never reads `approval.publication` (`rows.test.ts` pins that): "published on
your event stream" is the approval's own sentence, and delivery is the next one.

### The dashboard gets a count and a link, and not a word of the text

The same reasoning as the pending chip, one screen further out: the home screen
is what gets unlocked on a train, and an approval queue shows proposed text. So
`GET /api/suggestions` is reduced to two numbers in `$lib/approvals/summary.ts`
— a count and whether the count is a floor — and `$lib/dashboard/load.ts` keeps
nothing else. `summary.test.ts` asserts the property on the value; the journey
asserts it on the rendered page.

### The picker describes where the user is, and asks before it says

Screen 3 read "Connect your first network — step 1 of 3" to an owner with
WhatsApp connected for two hours and Signal for two minutes (#120). The
onboarding wizard's copy was shown unconditionally to anyone who reached the
picker. The Gateway knows how many networks are connected; the screen did not
ask.

It asks now, through `connection.ts`, and has three modes rather than two: with
nothing connected it is step 1 of onboarding, with something connected it is
"Add a network" with no step counter and the way back to the dashboard, and with
an unanswered `GET /api/bridges` it is the second — because "I could not ask" is
not "nothing is connected", and the heading that can be false is the one
withheld. The branch is never read from browser storage: spec #65 derives
onboarding progress from what exists on the Gateway.

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
is running its own suite.

### The suite cannot adopt a server it did not start

`reuseExistingServer` used to make that a hope rather than a fact, and on
2026-09-19 it cost a day (#185). A `serve-like-gateway.mjs` left on port 4319 by
a run started **twenty-eight hours earlier** was reused, every spec met the
previous day's export, and thirty-six failed — including `serving.spec.ts`
reporting a prerendered page as a 404, which is an assertion with nothing to do
with any recent change. The natural response is to look for the defect in the
diff, and the defect was not there.

So a build has a **name**: `tests/build-id.mjs` hashes every file of `build/` by
path and content, `tests/serve-like-gateway.mjs` publishes the name it is serving
at `/__twalk_test__/serving` — the one route here that is *not* transcribed from
the Gateway — and `tests/port-guard.mjs` consults it for all three origins before
anything starts. Three outcomes, and the middle one is why this is not simply
"fail when the port is busy":

- **free** — nothing to say;
- **a server serving this very build** — adopted, and the run says so, because
  the name is the export's own content and "serving this build" is therefore a
  fact. That is what keeps `npm run test:e2e` twice over one build cheap;
- **anything else** — the run stops before a single assertion, naming the port,
  the pid and command that holds it, when that process started, which build it
  is serving and which build is under test. And it names `TWALK_TEST_PORT`,
  because a hard stop for everyone whenever somebody leaves a server behind is
  the other way to get this wrong.

Under `npm run test:e2e:stack` nothing is adopted at all: each origin has a
Gateway and a Synapse of its own behind it, so a server already listening has
none however right its build is — and the guard says *that* rather than letting
Playwright refuse the port without explaining it.

The guard runs from `playwright.config.ts`'s own body, which is the last place
early enough: Playwright starts `webServer` **before** `globalSetup`, so a global
setup would be told about the stale server after the stale server had already
been adopted. `tests/e2e/ports.spec.ts` proves the diagnosis — it stands a decoy
server up and asserts what the guard says — and it needs no stack, so it runs in
CI's required tier where a defence like this has to live if it is not to rot.

The other half is that this server does not outlive its run: it stops on
`SIGINT`, `SIGTERM` and `SIGHUP`, taking the Gateway and the stub bridge with it,
and it exits on its own when the process that started it is gone. An orphaned
test server is the leftover nobody will remember, and two seconds' granularity is
nothing against twenty-eight hours.

The **approval journeys** (`tests/e2e/approvals/`) run as their own Playwright
project on the bridge Gateway's origin, after the `dashboard` project: approving
needs a signed-in device, a bus to publish a suggestion on and the Gateway's own
consent journal to read, and that Gateway is the only one configured with all
three. The suggestions are published onto the bus by the test rather than
produced by a persona — Hermes has its own suite against its own process
boundary — and everything downstream of the publish is real: the Gateway scans
the stream, checks its journal, and publishes `persona.reply.approved.v1`, which
the journey reads back off NATS. The trigger is deliberately loaded with a body,
a display name and a network identifier, and every screen the journey visits is
searched for all three.

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
  and the same stub bridges, on a deployment whose **device token lives fifteen
  seconds**. The ticket asks for the expiry to be arranged at the Gateway
  rather than waited for, and `GATEWAY_DEVICE_TOKEN_TTL` is the operator's own
  knob for it. It cannot be the bridge origin's, because that lifetime is
  deployment-wide and every other spec there would spend its life mid-expiry.
  Fifteen rather than five is #186, and the reason is in the next section.

One Gateway cannot be all three, which is the whole reason for the extra
origins. Everything else is shared: one orchestrator, one `proxyToGateway` in
`serve-like-gateway.mjs`, one `TWALK_TEST_REAL_STACK=1` gate. The bridge and
session origins also expose the stub's control surface at `/stub-control/*`, so
a browser journey drives both sides of a login — the user's and the bridge's —
without knowing a second port.

The bridge is stubbed for the reason spec #47 gives: a real mautrix-whatsapp
needs a live WhatsApp account and a human with a phone. Nothing else is stubbed.

### The session journeys have a tolerance, and it is the lifetime

These four journeys are the only ones in this suite whose subject is *when*
something happened, so they are the only ones a slow machine can beat. #186 is
what that cost, and the fix is one number plus one place in the ordering.

The client renews a token with a fifth of its lifetime still in hand
(`refreshAfterSeconds`). That fraction is right for a deployment — a
fifteen-minute token is renewed after twelve minutes, three whole minutes of
slack — and it means the **absolute** headroom a test gets is a fifth of whatever
this origin is configured with. At five seconds' lifetime that was one second,
which is less than a browser timer slips on a machine that is linking a Rust
binary. The suite was racing the mechanism it exists to observe, and it lost
differently every time, which is exactly how a flaky suite teaches people to
re-run instead of to look.

Fifteen seconds buys three, stated as the tolerance in
`tests/e2e/session/harness.ts` and derived there rather than written into each
spec. It costs about forty-five seconds and changes nothing about what is proved:
the same rotation, the same `expires_in`, the same expiry arranged at the Gateway
— what #111 refused was waiting the fifteen real minutes, not choosing a number.

Two smaller things in the same ticket, and both were the suite measuring itself
rather than the product:

- **the death of a credential is asked about, not timed.** A spec that needed a
  dead token used to sleep for the configured lifetime, but the page rotates its
  token whenever it likes, so "a lifetime from now" was never a bound on when the
  one it holds expires. When the guess fell short the next click simply worked,
  no `401` arrived to be repaired, and the spec failed as though the repair were
  broken. It now polls the Gateway from inside the page until the credential is
  actually refused.
- **"a sleeping tab refreshes nothing" counts requests.** It is a statement about
  a timer not firing, and the old assertion counted answers — so a refresh
  already in flight when the clock froze had its answer arrive during the sleep
  and was counted against the tab.
- **the journeys have a budget of their own.** They had Playwright's default
  thirty seconds, which was quietly carrying an eighteen-second deliberate wait
  plus a real-stack sign-in, an app load and two screens — a margin nobody had
  chosen, in the one project whose specs are allowed to be slow. When it ran out
  the failure was a *timeout* on a spec about keeping sessions alive, which says
  nothing about what went slowly. `journeyBudgetMs` states it. The `approvals`
  project had the identical defect and it was worse there: every one of its specs
  opens with a thirty-second poll inside that thirty-second budget, so the poll
  could never use its window at all.

#### A rotation is a moment a request must not cross

Found while measuring the above, and worth knowing because it is the product and
not the fixture. `Sessions::refresh` overwrites `device_token_sha256`, so the
previous device token stops working the instant the new pair is issued —
deliberately, so a token that leaked dies at the next refresh rather than living
out its lifetime. A request that crosses a rotation therefore carries a
credential that has just stopped being one, is refused, and is repaired centrally
by the belt. In a deployment that is one rotation every twelve minutes against a
round trip of milliseconds; here it is one every twelve seconds.

Three lifetimes is *exactly four* renewal intervals on this origin, so the first
journey was clicking into a rotation on most runs and failing on an overlap it
had arranged itself. It now waits for every renewal due in those three lifetimes
to have landed — a stronger statement than the "at least three" it replaced,
since a missed renewal covered for by the `401` repair would satisfy the old one
— and then clicks with a whole interval of runway, which is what a user has.

And it keeps **no `dependencies`**, which was reconsidered rather than assumed.
Ordering it last looked right — it runs beside `consent`, whose `beforeAll`
compiles the Sensor and then runs it against a real homeserver with a crypto
stack, and a browser measuring three seconds' headroom while the same machine
does that is measuring the load this suite creates for itself. It was tried, and
measured, and undone: with the lifetime and the waits fixed these four journeys
pass on a host pinned at load 25–30, so they do not need an idle machine — and a
dependency is not free, because a project whose dependency fails does not run at
all. Behind `portals` the one suite that proves an expired session is refused
went silent the moment an unrelated project went red (#211), which is how a
suite stops being believed rather than how it starts. Running it alone is
`--project=session`.

### The consent journey runs a real Sensor, and it is the only one that does

`tests/e2e/consent/` starts the actual `twalk-sensor` binary beside the bridge
origin's stack (`tests/e2e/consent/sensor.ts`), because #170's keystone
criterion is a fact about a *label* and the Sensor is what stamps it. It logs in
to the real Synapse, joins a real room on the owner's invitation, reads real
messages and publishes to the real JetStream the Gateway's outbox publishes the
decision to. `SENSOR_GATEWAY_URL` is left unset on purpose: the cache then starts
cold, everything is `pending`, and the consent consumer is still created — so a
`granted` label is evidence about the decision and about nothing else.

Two costs worth knowing. A cold `cargo build` of the Sensor is minutes
(matrix-sdk and its crypto stack), which is why that project's timeout is
generous. And the Sensor's consent consumer is a **durable with a constant
name**, so two Sensors on one bus split the consent stream between them: this
project runs one worker, reaps the process in `afterAll` whether it passed or
not, and must not run beside `sensor/`'s own Cargo suites.

The `consent` project depends on `approvals` (and so on `dashboard` and
`networks`) for the reason those depend on each other: one Gateway, one consent
journal, one pending-contact projection. It writes decisions and makes a contact
pending, which is state the dashboard's counts are asserted against.

#### A contact is never a Matrix ID on its own

That is #200, and the interesting part of it is that the screen was right all
along. *"Returning a contact to undecided is a decision, not an erasure"* failed
on both attempts, eight specs behind it never ran, and both the screen and the
Gateway were doing exactly what ADR 0010 asks: the decision was recorded, the row
kept `decidedBy: 'contact'`, and `model.ts` expresses the distinction between
"decided pending" and "never decided" exactly as #170 built it to.

What was wrong was the question. Consent is keyed on `(subject, network)` — the
perimeter is half of the fact — and this spec asked the Gateway "is this contact
waiting for a decision?" by flattening the pending list to Matrix IDs. On this
project's shared test stack `@bot_beta:test.twalk` is the consent journey's
contact on `matrix` **and** a WhatsApp sender in half of `sensor/tests/`, on one
JetStream every suite publishes to and that the Gateway's pending-contact
projection replays from the beginning on each run. So a `matrix` decision left a
`whatsapp` row waiting — correctly, and `companion-gateway/tests/pending.rs`
asserts precisely that — and the flattened answer stayed `true`. Whether the spec
passed depended on whether the Sensor's suite had ever run against that stack,
which is why one report said 52 specs passed and CI's said one failed twice.

Three things changed, and each proves more than before:

- the pending list is read **per network**, the same question
  `pending.rs::waiting_networks` asks the same endpoint;
- a bus event is matched on subject **and** network, because the contract's own
  inbound fixture carries `consent: granted` and another suite's event about the
  same account would otherwise satisfy an assertion that *this* conversation's
  message is labelled `pending` — which is the same cause wearing the other face,
  and the intermittent first spec of that file;
- the second perimeter is **arranged** rather than inherited: `beforeAll`
  publishes a WhatsApp sighting of the same contact, so the journey now asserts
  that the `matrix` decision answers for `matrix` and leaves `whatsapp` waiting.
  That is what a perimeter *is*, and it no longer depends on the stack's history.

One more thing that file got wrong and that produced the same misleading shape:
`test.setTimeout` at describe level applies to the tests, not to `beforeAll` — so
the hook that builds the Sensor, logs it into a homeserver and waits for it to
accept an invitation had the default thirty seconds. When it runs out, Playwright
reports the first spec as failed and every other one as never run. The hook now
states its own budget.

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

The homeserver field starts **empty**, and that is a decision rather than an
omission (#124). It used to arrive holding the Twalk deployment's own
homeserver, which is the one account this screen is not for; since the screen
reads the login flows of whatever is in the field, the local deployment's
password form was offered to an owner whose account signs in through their
organisation's identity provider, and nothing suggested their homeserver was
supported at all. The only value ever offered is the homeserver of an account
this browser has already connected here.

It accepts a **server name or an address**, because `.well-known` delegation
exists so that `linagora.com` — the half of a Matrix ID a person can recite — is
enough. `discoverHomeserver` does both: a bare name is resolved as screen 1
resolves one, a value carrying a scheme is taken as the address it is (port and
path included), and the screen says where a delegation led, since the
credentials are about to go somewhere the user did not type.

Sign-in covers SSO as well as a password, and offers one button per advertised
identity provider, labelled with the provider's own name and redirecting to
`/sso/redirect/{idpId}`: a homeserver with SSO usually has *only* SSO, and a
password form there is a dead end. The screen also states the return address the
homeserver will be asked to use: a production Synapse with an
`sso.client_whitelist` refuses one it has not been told about, which is not
Twalk's to fix but is the user's to be told about.

The `loginToken` the homeserver redirects back with is a short-lived credential,
so it is taken out of the address bar at once — a copied or bookmarked URL must
not carry one. That happens in `$lib/matrix/login-token.ts`, called while the
root layout initialises, and **not** in this route: the route did not always
mount (the tab-lock screen rendered instead) and the credential stayed in the
URL. A precaution that only runs when the page it guards is allowed to run is
not a precaution (#135).

### The conversation chooser is a third unit of decision, and it states its cost

`/networks/conversations` (#143) asks one question per conversation: *do I watch
this room.* It exists because the two units Twalk already had are not the unit a
user thinks in here. For `maria` a contact and a conversation are the same thing
and consent-per-contact works; for `Échecs en Yvelines` at 246 members nobody
adjudicates 246 people one by one, and ticking that row is a decision about 246
people in one gesture. Observation and consent compose rather than compete: an
unwatched conversation produces nothing, and inside a watched one each sender's
consent still governs what is published about them (ADR 0012).

Four things about it are decisions rather than layout.

**The kind is read, never guessed.** Every portal carries the network's own
identifier for the conversation (`network_conversation_id`, the `m.bridge`
`channel.id` passed through untouched by the Gateway) and its suffix is the
network's own statement: `@lid` and `@s.whatsapp.net` are one person, `@g.us` a
group, `@newsletter` a broadcast. A conversation whose bridge wrote no
identifier is `unstated` and gets a section saying so — never inferred from the
member count, which is evidence about a conversation's size and not its type.

**Communities are grouped by name, and the screen says that is what it did.** A
WhatsApp community is, at the network level, a set of ordinary `@g.us` groups,
and no field anywhere says which groups belong to one. What the register can see
is what the owner saw when they worked those eighteen rooms out: a community
arrives as several groups whose names contain one another — `Communauté CKCP`
twice in the same minute at 109 members and 6, `XVDSI` and `XVDSI - General`.
So `families()` clusters on whole-word containment of one folded name in
another, and the screen states plainly that the network never told it which row
is the parent. Each row keeps its member count and its network address, which is
what tells two rows called `XVDSI` apart at all. Its limit is stated too: a
subgroup with an unrelated name is indistinguishable from an ordinary group and
appears as one.

**The consequence is stated before the tick takes effect.** Every row carries
its count, a community carries the total across its conversations *in the label
of the control that ticks it*, and the pending decision is costed in people
against the whole account rather than against what the search left on screen.
Past `CROWD` — twenty, which sits between the largest of those eighteen
conversations a user could have named person by person (eleven) and the smallest
that is unmistakably a crowd (seventy-three) — the decision cannot be sent until
the number has been acknowledged, and changing the selection asks again. A
number is not a warning, and a warning nobody reads is not a decision (#122).

**The bulk control and the search are `$lib/matrix/rooms.ts`'s.**
`scopedBulkControl` and `matchesQuery`, the same implementations the Matrix room
chooser obeys (#137) — the Matrix screen was moved onto the shared one in the
same change. "Select the 4 shown", never "select all", over a list that can
contain a 246-member association. A second spelling of a rule like this is how
#110 happened.

An empty list is never rendered as "you have no conversations": the register
names every configured bridge, whether it could be read, which account it was
read as and how many rooms that account is in (#171), and the screen renders a
bridge with no token and a bridge whose asker is in no rooms as the two
different things they are.

### One component owns a login's flow, and a table owns its steps

A bridge decides how many steps a login has, what each asks for and in what
order, and it does not know in advance: a two-factor password step appears only
for an account that has one. So `BridgeLogin.svelte` owns the **flow** — which
bridge, the disclosure, the polling, the refusals about the deployment, the
explicit abandon — and delegates each kind of step to a panel of its own
(ADR 0030).

The dispatch is a table typed over the view's own discriminant
(`login-panels.ts`, `components/login/panels.ts`), so a kind added to `LoginView`
and drawn by nobody is a **missing property**, which is a compile error. That
sentence used to be a comment in `login-view.ts` claiming a guarantee the
language does not give: a Svelte `{#if}` chain receives no exhaustiveness check,
the QR screen had no `{:else}`, and an `input`, `cookies` or `emoji` step drew an
empty seventeen-rem box that the session polled once a second for ever — which
is where a Telegram QR login by an account with two-factor authentication ended
up. One panel is the residual, for the case no type can remove: a Gateway newer
than this app, answering a step type that did not exist when the app was built.

Inside a step the renderer is driven by the **field type** (`step-fields.ts`).
Ordinary fields get one control each; fields the bridge groups by type — a jar of
`cookie` fields sharing a domain — are collected through one control and its
parser, because people arrive at that step holding a whole `Cookie` header, not
seven values to transcribe. A type this build does not know, **or one the bridge
did not declare**, refuses that field and names it on screen: the alternative,
and the previous behaviour, was a password field whose type a bridge had omitted
drawn as a plain text input.

The known types come from **bridgev2's own enumerations** — `mautrix/go`,
`bridgev2/login.go`'s `LoginInputFieldType` and `LoginCookieFieldSourceType` —
and not from the types this repository has met. That distinction is the whole
difference between refusing a field nobody can draw and refusing a legitimate
login: the first version of this module knew one grouped type, `cookie`, because
that is the one #57's screen was written for, and a first-party connector
(LinkedIn) asks for three `request_header` fields. So when a type appears that is
not there, the fix is to read the enumeration again rather than to add the one
value in front of you.

The two field documents are not the same shape, which is the thing to know
before reading the parse. A `user_input` field carries its `type` itself; a
`cookies` field carries **no type at all** — it has `sources`, a list, each with
a type, the `name` the value goes by in the browser and a `cookie_domain` — and
it carries `required`. Three consequences: the answer is keyed by the field's
`id` while the paste is keyed by the source's `name`, and only the id will do;
all five grouped source types share one control, because bridgev2's own answer
to a `cookies` step is one map whatever each field's source was; and a field the
bridge called **optional** never blocks the step — it is left out of the answer
and said so on screen, because a field that vanished silently is a field the
user goes looking for.

A step is **named, never counted** — a bridge cannot say how many remain — and a
**refused answer is its own outcome** rather than a banner over the step. The
bridge destroys the login process when it declines a value, so the step is
removed instead of inviting a resubmit that could only `404`, and the screen
carries the Gateway's own sentence, which names the network's code. Before this,
`invalid_request` was absent from the trouble table and a network refusing a
phone number was reported as *"something went wrong on your Twalk server"*.

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

Since ADR 0030 the screen is the shared one and the cookie step is drawn by the
same field-driven panel every step goes through; what was bespoke about it was
the **prose**, and the prose is `stepCopy` in `copy.ts`, keyed on the bridge's own
step id and **replacing** the bridge's eleven words rather than sitting above
them. That keying can rot — a bridge that renames a step takes its explanation
with it — so the panel declares the shape it was written against and says so
loudly, falling back to the bridge's own words, when the step stops matching. A
cookie step nobody wrote about says that too: a credential handover explained
only by its bridge is named as such rather than drawn quietly.

## What is not here yet

Screen 1's secondary "pair with my other device" link waits on the
device-pairing flow and is not wired. MSC4108 sign-in by QR from another Matrix
client is named on screen 3d rather than offered: it carries encryption secrets
across and needs the crypto stack these screens do not load. Messagr pairing is
shown on the dashboard as unavailable in v0.1, which it is.

### A collector connection's card says what the collector said, in four sentences (#275)

The owner's mailbox and calendars are connections the collector holds (ADR 0033), and since #275 each has a card on the networks screen like a bridge's connection has — `email` and `calendar` in `catalogue.ts`, `collector: true`. Three decisions in it.

**The card has no route.** A bridge's card leads to a login journey because the browser can complete one: a QR code, a phone. A collector's grant is given by the operator at the server's terminal (`provision-connection.sh`), and nothing a browser could offer would be the truth about it — so the card reads, and does not lead. The one action it names is the collector's own hint, shown under the card.

**Its badge is what the collector said, as four sentences and never one.** The state comes from `GET /api/connections`'s `status`, which the Companion Gateway read off `connection.status.changed.v1`; `connected` is the tick, `unreachable` a service not answering and retried by itself, `reconnect_required` and `pending_operator` the operator's — the grant to give again, the client to change — and those two look like warnings. A connection whose collector has not spoken is `unknown` and wears no badge, the same honesty as the bridge link's `unknown` in `connection.ts` and for the same reason: it is the state of our knowledge. A `link` a bridge would have is left `unknown` on purpose; `connected` on the card is the collector's word alone.

**The feed says a connection moved; the card says what to do.** The dashboard's activity feed carries every transition (`dashboard.feed.connection.*`), by connection id and state, and never the hint: the operator's sentence is one thing to read, on one screen, and the feed names no person — a connection's id is configuration.

The Gateway's refusal `connection_not_connected` — an approval towards a connection that cannot send — is a sentence in the five catalogues like every other code (`refusal.ts`), and the clerk's table carries it too, both held to the OpenAPI by their tests.
