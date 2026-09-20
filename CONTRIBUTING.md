# Contributing to Twalk

Twalk accepts contributions under the terms of the Developer Certificate of Origin: by signing off a commit (`git commit -s`) you state that you wrote the patch or have the right to submit it under the project's licence.

Read [`CONTEXT.md`](CONTEXT.md) before writing anything. It is the project's glossary and it is enforced, not advisory: a pull request that calls a network a "channel" or an agent identity a "bot" will be asked to change its words. [`AGENTS.md`](AGENTS.md) holds the build and test commands, and [`docs/architecture/adr/`](docs/architecture/adr/) records why the structural choices are what they are — including the ones that look surprising.

## How work is organised

The issue tracker is GitHub Issues on [linagora/twalk](https://github.com/linagora/twalk/issues), and its conventions are in [`docs/agents/issue-tracker.md`](docs/agents/issue-tracker.md). In short: a component's spec is a parent issue labelled `spec`, its tickets reference it, blocking relationships live both in the ticket body and as native GitHub dependencies, and a ticket labelled `ready-for-agent` is self-contained enough to be picked up cold.

Work reaches `main` through a pull request whose body carries `Closes #N`, a summary, and the test evidence — the actual output, not a claim. Nothing merges on a red suite.

## What verifies your pull request

Opening a pull request gets you a verdict without anybody running anything by hand. Three checks decide it — **`routing`**, **`verified`** and **`verified-stack`** — and there are three rather than one because "the suites passed" and "the suites never ran" must not share a tick. `docs/agents/continuous-integration.md` is the full account; four things are worth knowing before you open one.

**Running the suites locally is no longer the verification, and it is still the fastest way to find out whether your change works.** The difference matters because it is what two red-`main` incidents cost: both were a change to a shared path merged on the strength of "I ran the suites", where the suite that broke belonged to a different component. So keep pasting the output — a reviewer reads it — but `verified` is what says the change is sound.

**A change to a shared path runs the consumers' suites.** `tests/harness/` is a dev dependency of five components (the Sensor, Hermes, the Companion Gateway, the clerk, the collector) and the Companion's real-stack e2e brings up its compose file; `contracts/cloudevents/v1/` is read by the harness's validator, the Sensor and the SDK's tests; `companion-gateway/openapi.yaml` is what the Companion's API client is generated from. That routing lives in `.github/ci/suites.json`, and `.github/ci/test_selection.py` derives each shared path's consumers from the repository rather than trusting the table — so if you add a dependency, the routing check will tell you to route it.

**The deployment suites are advisory and opt-in.** The ones that raise the whole reference deployment run nightly on `main`, and on your pull request only if you add the **`ci:deployment`** label. Add it when you touch `deploy/docker-compose/` or `bridges/`.

**Do not re-run a red required check until it passes.** No suite with an open flake ticket is in the required tier, so a red one means something: your change, or a new flake that is worth a ticket of its own. If you do re-run, say so in the pull request and say how many times. A check people learn to ignore is worse than no check.

## Tests

Tests live at the process boundary: a real homeserver, a real bus, the real binary, and assertions made from outside. `docs/architecture/roadmap.md` explains which lot owns what, and each component's test module documents the environment variables that isolate its stacks and ports — set them, because several suites can run at once on one machine.

Two things about the stack itself, learned expensively. **The shared test stack is meant to stay up between runs**, and one suite currently needs it to: `sensor/tests/bridge_bots_are_not_contacts.rs`'s presence assertion fails on a Synapse that has just been created and passes on one that has already served a suite — measured twice each way while CI was being built. So `docker compose … down -v` before a run is not a way to get a clean result; it is a way to get a red one. And **a server left listening is worse than one that is missing**: a `serve-like-gateway.mjs` on port 4319 from the previous day made the Companion's suite test a day-old build and fail thirty-six tests as though the code were broken (#185). Before believing an e2e failure, check what holds `TWALK_TEST_PORT` and the two ports after it.

Two habits the project has learned the hard way. Assert the *absence* of things as deliberately as their presence: several of Twalk's promises ("no body reaches the bus for a revoked contact", "no LLM call for an unconsented message", "no recovery key leaves the browser") are only real because a test looks for the thing in every request, every log line and every stored byte. And prove a property against the system's own state rather than the interface: ask the homeserver whether a room was joined, ask the bus whether an event was published.

## Translations

The Companion ships in English, French, Italian, Spanish and German. Catalogues are ICU MessageFormat JSON at `companion/src/lib/i18n/<locale>.json`, and each carries a header — its first entry, `catalogue.review` — stating whether a native speaker has reviewed it. As of this writing, **English and French are reviewed; Italian, Spanish and German are not** — they were produced without a native reviewer, and improving them is one of the most useful contributions available.

How it behaves, so a partial contribution is safe: a key missing from a catalogue falls back to English at runtime, and a test fails when a catalogue's keys have drifted from English's — missing or extra — or when a string's placeholders no longer match the English string's. So you can contribute a language incrementally, and the suite will tell you exactly which keys remain rather than letting a catalogue quietly rot at sixty percent.

To add a language: copy `en.json`, translate what you can, state your review status in the header, and open a pull request. Say in it whether you are a native speaker of the language — that is the information the project cannot get any other way.

Two rules that matter more than they look. Keep ICU placeholders exactly as they are (`{count}`, `{domain}`): they are substituted at runtime and a renamed one breaks the string. And translate the *meaning*, not the words — several strings deliberately say uncomfortable things ("Twalk never sees it", "there is no reset in this version"), and a softer translation would make the product lie in your language.

Prompts sent to a language model stay in English, and a persona answers in the language of the message it is answering (ADR 0016). That is a separate mechanism from the interface's translation, and it needs no translated string.

## Reporting a security issue

Do not open a public issue. Follow the process in `SECURITY.md`, and read [`docs/architecture/security-model.md`](docs/architecture/security-model.md) first — it states plainly what Twalk protects, what it does not, and which residual risks are accepted, so you can tell a finding from a documented trade-off.

## Interoperability

Contributions that improve interoperability with adjacent open source projects — Pimalaya, Matrix, Mautrix, CloudEvents tooling — are especially welcome and reviewed with priority. Twalk's default posture towards a mature building block is to adopt it, contribute upstream, and preserve its interface rather than fork it; a patch that moves us further in that direction is doing the project's strategy, not just its backlog.
