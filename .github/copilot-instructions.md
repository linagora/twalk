# Reviewing a pull request in this repository

Twalk is a sovereign, self-hosted event hub: personal messaging lands in Matrix portal rooms, the Sensor normalises it into versioned CloudEvents on a NATS JetStream bus, and Hermes-hosted personas reason over it under human oversight. Read `CONTEXT.md` for the vocabulary and `AGENTS.md` for how each component is built and tested. `docs/architecture/adr/` holds twenty-seven decisions and is the reason most of the surprising code is the way it is.

The review culture here is **argue with the decision**. Pull requests in this project routinely run to three or four thousand lines and carry their own rationale, an "argue with this" section, and often an ADR. A comment that says "consider extracting this into a helper" is worse than silence, because it costs a reader's attention and teaches them to skim your comments. A useful comment does one of exactly two things:

- **disputes a decision on its merits** — names the alternative, says what it would cost, and says why the one taken is wrong; or
- **names a concrete failure scenario** — the inputs, the configuration, the sequence, and what the system would then do.

If a comment fits neither shape, do not post it.

## The defect this project keeps shipping: two different failures behind one signal

This is the single most repeated bug class in Twalk's history, it has produced a separate issue nearly every time, and a reviewer that learns to look for it earns its place. The shape is always the same: one value, one log line or one silence stands for two facts that need different actions, and nothing can tell them apart.

- **`absent: 0` meant two things** (#171). The portal register asked the homeserver with an appservice token and no `?user_id=`, which acts as the registration's generated `sender_localpart` — an account joined to nothing. Against 32 real conversations it answered `observing=0 invited=0 absent=0 unreadable_bridges=0`, which is also exactly what a correct register answers on a fresh deployment where no conversation has become active yet. The fix was not only to ask as the right account: each bridge's reading now carries `asked_as` and `joined_rooms`, so a zero says which zero it is.
- **A `401` rendered as "the server is unreachable"** (#111, #135, #139 — three incidents in one day). "Nothing answered" and "answered, and would not accept this session" send the user to look at two completely different things; conflating them sent them to inspect a firewall over a sign-in. `companion/src/lib/api/trouble.ts` exists, three words long, to keep them apart, and `Explained` deliberately has no value meaning "still working" so that a refusal cannot render as a spinner.
- **A stale server made a day-old build look broken** (#185). A `serve-like-gateway.mjs` left listening on port 4319 from the previous day meant Playwright tested yesterday's export; 36 tests failed as though the code were wrong. "The code is broken" and "you are testing something else" were one red suite.

So: when a diff adds a count, a boolean, an empty answer, a `catch` that logs one message, or a default that stands in for a missing value, ask whether two different situations now produce the same output, and whether the one that matters is the silent one. Twalk's worst defects have all been silences — a conversation that never reached the bus, a preference the Companion confirmed and nothing applied — so a change that makes a failure *quieter* deserves a comment even when it is otherwise correct.

## Rules that are not style preferences

**Never suggest running a formatter.** There is no Prettier, no rustfmt-on-save and no equivalent configured anywhere in this repository, and none is wanted. The TypeScript and Svelte style is tabs, single quotes and a ~100-column wrap, maintained by attention; the Rust was written to `cargo fmt` defaults. `npx prettier --write` here resolves to whatever binary is on the machine, applies *its* defaults and rewrites whole files: it once turned a 200-line change into 1560 insertions and 1464 deletions, which had to be undone by hand. Do not suggest adding a formatter, a lint config or a pre-commit hook that runs one.

**A change to `contracts/` or `tests/harness/` must land with its consumers' suites run, not only its own.** `tests/harness/` is a dev dependency of `sensor/`, `hermes/` and `companion-gateway/`, and `companion/`'s real-stack e2e brings up its `compose.test.yaml`; `contracts/cloudevents/v1/` is read by the harness's validator, by the Sensor and by the Python SDK's tests. `main` has gone red twice for exactly this reason — a shared change merged after only its own component's suite had run. The same applies to `companion-gateway/openapi.yaml`, which the Companion's generated client is derived from: a change there without `npm run api:check` in `companion/` is a stale client waiting to happen. If a pull request touches a shared path and its body does not show the consumers' suites, say so and name which ones are missing.

**An ADR that states a fact must have the fact verified.** A decision that is hard to reverse, surprising without its context, and the result of a real trade-off gets an ADR in `docs/architecture/adr/`. Review the reasoning, and then review the *claims* separately: an ADR merged in this repository asserted that a bridge's bot is the appservice's `sender_localpart`, which is false on any generated registration, and that is precisely the false belief that had caused #171 three hours earlier. When an ADR or a doc says "X is Y" about Matrix, mautrix, Synapse, NATS or CloudEvents, ask how it was checked and whether a test observes it. A wrong fact inside a correct decision is the expensive kind.

**`Closes #N` goes in the pull request body as plain text on its own line, never inside backticks** — GitHub does not link it otherwise, and the issue stays open after the merge.

**The vocabulary in `CONTEXT.md` is enforced, not advisory.** A **network** is WhatsApp, Signal, SMS, Telegram, Discord or Matrix — never a "channel" outside user-facing copy, and never a bridge name like `gmessages`. A **persona** is never a "bot" (a *bridge bot* is a different thing and is not a person, ADR 0026). The **Companion** is the PWA; the **Companion Gateway** is always written in full. This is worth a comment when a diff gets it wrong, because the words are how four components stay agreed.

**Absence is a first-class assertion here.** Several of Twalk's promises — no body reaches the bus for a revoked contact, no LLM call for an unconsented message, no recovery key leaves the browser, no credential reaches a persona's environment — are only real because a test looks for the thing in every request, every log line and every stored byte. If a diff adds a behaviour whose value is that something does *not* happen, and the test only asserts that the happy path still works, that is the comment to make.

**Consent and message content.** No message content may leave the user's infrastructure without explicit consent, and an excerpt of a quoted message belongs to its author rather than to whoever sent the event carrying it (ADR 0012). A diff that widens what a projection, a listing or a log line holds about a contact — a display name, a body, a `network_identifier` — is worth stopping over even when it looks like a convenience.

## What not to comment on

- Formatting, whitespace, import order, line length, or any naming preference.
- Test coverage expressed as a number, or a request for "more tests" without naming the case that is missing.
- Anything the diff's own prose already argues. These pull request bodies are long and usually contain the objection you are about to raise, together with why it was decided the other way. Read the body first; if it addresses your point and you still disagree, argue with *its* reasoning rather than restating the point.
- Suggestions to add a dependency, a framework, a formatter or a linter. This project's posture towards a mature building block is to adopt it upstream and preserve its interface rather than fork it — and towards anything else, to not add it.
- Documentation phrasing, unless the sentence states something factually untrue.

A review that posts nothing on a pull request whose decisions are sound is a correct review.
