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
| `src/lib/version/` | The version handshake against the Gateway's `/health`, and the reload it forces. |
| `src/lib/i18n/` | French and English, ICU patterns, `<locale>.json` per the wireframes. |
| `src/lib/icons/` | The one module that imports an icon library. |
| `src/lib/styles/` | `tokens.css` (the design tokens) and `base.css` (element defaults). |
| `src/service-worker.ts` | Installability. Caches the fingerprinted build and nothing else. |
| `tests/serve-like-gateway.mjs` | The Gateway's own path resolution, in Node, for Playwright. |

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

## What is not here yet

Screens 3 to 5 of the wireframes: the network flows (#68), persona activation
and the dashboard. Screen 1's secondary "pair with my other device" link waits
on the device-pairing flow and is not wired, and the Matrix access token the
network screens will need is kept in memory only (`$lib/onboarding/progress.ts`)
— nothing sensitive goes into browser storage, so a reload loses it and those
screens will obtain it again rather than find it lying about.
