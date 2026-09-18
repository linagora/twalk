# Contributing to Twalk

Twalk accepts contributions under the terms of the Developer Certificate of Origin: by signing off a commit (`git commit -s`) you state that you wrote the patch or have the right to submit it under the project's licence.

Read [`CONTEXT.md`](CONTEXT.md) before writing anything. It is the project's glossary and it is enforced, not advisory: a pull request that calls a network a "channel" or an agent identity a "bot" will be asked to change its words. [`AGENTS.md`](AGENTS.md) holds the build and test commands, and [`docs/architecture/adr/`](docs/architecture/adr/) records why the structural choices are what they are — including the ones that look surprising.

## How work is organised

The issue tracker is GitHub Issues on [linagora/twalk](https://github.com/linagora/twalk/issues), and its conventions are in [`docs/agents/issue-tracker.md`](docs/agents/issue-tracker.md). In short: a component's spec is a parent issue labelled `spec`, its tickets reference it, blocking relationships live both in the ticket body and as native GitHub dependencies, and a ticket labelled `ready-for-agent` is self-contained enough to be picked up cold.

Work reaches `main` through a pull request whose body carries `Closes #N`, a summary, and the test evidence — the actual output, not a claim. Nothing merges on a red suite.

## Tests

Tests live at the process boundary: a real homeserver, a real bus, the real binary, and assertions made from outside. `docs/architecture/roadmap.md` explains which lot owns what, and each component's test module documents the environment variables that isolate its stacks and ports — set them, because several suites can run at once on one machine.

Two habits the project has learned the hard way. Assert the *absence* of things as deliberately as their presence: several of Twalk's promises ("no body reaches the bus for a revoked contact", "no LLM call for an unconsented message", "no recovery key leaves the browser") are only real because a test looks for the thing in every request, every log line and every stored byte. And prove a property against the system's own state rather than the interface: ask the homeserver whether a room was joined, ask the bus whether an event was published.

## Translations

The Companion ships in English, French, Italian, Spanish and German. Catalogues are ICU MessageFormat JSON at `companion/src/lib/i18n/<locale>.json`, and each carries a header stating whether a native speaker has reviewed it. As of this writing, **English and French are reviewed; Italian, Spanish and German are not** — they were produced without a native reviewer, and improving them is one of the most useful contributions available.

How it behaves, so a partial contribution is safe: a key missing from a catalogue falls back to English at runtime, and a test fails when a catalogue's keys have drifted from English's — missing or extra. So you can contribute a language incrementally, and the suite will tell you exactly which keys remain rather than letting a catalogue quietly rot at sixty percent.

To add a language: copy `en.json`, translate what you can, state your review status in the header, and open a pull request. Say in it whether you are a native speaker of the language — that is the information the project cannot get any other way.

Two rules that matter more than they look. Keep ICU placeholders exactly as they are (`{count}`, `{domain}`): they are substituted at runtime and a renamed one breaks the string. And translate the *meaning*, not the words — several strings deliberately say uncomfortable things ("Twalk never sees it", "there is no reset in this version"), and a softer translation would make the product lie in your language.

Prompts sent to a language model stay in English, and a persona answers in the language of the message it is answering (ADR 0016). That is a separate mechanism from the interface's translation, and it needs no translated string.

## Reporting a security issue

Do not open a public issue. Follow the process in `SECURITY.md`, and read [`docs/architecture/security-model.md`](docs/architecture/security-model.md) first — it states plainly what Twalk protects, what it does not, and which residual risks are accepted, so you can tell a finding from a documented trade-off.

## Interoperability

Contributions that improve interoperability with adjacent open source projects — Pimalaya, Matrix, Mautrix, CloudEvents tooling — are especially welcome and reviewed with priority. Twalk's default posture towards a mature building block is to adopt it, contribute upstream, and preserve its interface rather than fork it; a patch that moves us further in that direction is doing the project's strategy, not just its backlog.
