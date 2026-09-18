# Roadmap — milestones and lots

The [README roadmap](../../README.md#roadmap) states **what** Twalk commits to and **when**: three product milestones (v0.1, v0.2, v1.0). This document states **how** that work is cut up and **where** it stands: the lots, their specs, their tickets, and the order they can be built in.

Two words, two different things:

- A **milestone** is a product commitment — a date and a user-visible scope. Milestones are owned by the README; this document never redefines them.
- A **lot** is a unit of execution — one component or one coherent slice of one, specced as a parent `spec` issue holding the full spec, with `ready-for-agent` tickets under it. A lot is done when all its tickets are closed and merged.

The **issue tracker is canonical** for status: GitHub Issues on [linagora/twalk](https://github.com/linagora/twalk/issues) (conventions in [`docs/agents/issue-tracker.md`](../agents/issue-tracker.md)). The tables below are a map, not a source of truth — when they disagree with the tracker, the tracker wins.

Status vocabulary: **done** (all tickets merged) · **in progress** (tickets open, work started) · **specced** (spec and tickets written, not started) · **planned** (no spec yet — writing it is the first step).

Last reviewed: 2026-09-17.

---

## Where we are

| Lot | Milestone | Spec | Status |
| --- | --- | --- | --- |
| Contract — CloudEvents v1 | v0.1 | — (landed with the docs seed) | done — 8 schemas, one validated fixture each |
| Sensor | v0.1 | [#1](https://github.com/linagora/twalk/issues/1) | done — tickets 01–11 merged, reviewed 2026-09-17 |
| Reference deployment (Compose) | v0.1 | part of the Sensor lot (ticket 11) | done for Synapse + NATS + Sensor; grows with each component |
| Hermes | v0.1 | [#19](https://github.com/linagora/twalk/issues/19) | in progress — H1 [#20](https://github.com/linagora/twalk/issues/20) and H2 [#21](https://github.com/linagora/twalk/issues/21) merged (the SDK with its consent gate, and the `assistant` skeleton), H3 [#22](https://github.com/linagora/twalk/issues/22) and H4 [#23](https://github.com/linagora/twalk/issues/23) the frontier; H4 absorbed the persona consent gate, H5 gained a current-state re-check |
| Human approval (Gateway + Companion) | v0.1 | #46 and #65 | specced — [#97](https://github.com/linagora/twalk/issues/97), [#100](https://github.com/linagora/twalk/issues/100) |
| Model, language and tracing configuration | v0.1 | #46 and #65 | specced — [#98](https://github.com/linagora/twalk/issues/98), [#101](https://github.com/linagora/twalk/issues/101), [#99](https://github.com/linagora/twalk/issues/99) |
| Five interface languages | v0.1 | #65 | specced — [#102](https://github.com/linagora/twalk/issues/102) |
| Matrix as a network | v0.1 | — (standalone tickets) | in progress — contract [#17](https://github.com/linagora/twalk/issues/17) merged, Sensor [#18](https://github.com/linagora/twalk/issues/18) open |
| Bridges | v0.1 | — (standalone ticket) | in progress — WhatsApp and Signal in the reference deployment ([#73](https://github.com/linagora/twalk/issues/73)); mautrix-gmessages (SMS) still planned |
| Companion Gateway — consent and auth | v0.1 | [#46](https://github.com/linagora/twalk/issues/46) | specced — tickets #48–#54 |
| Bridge provisioning facade | v0.1 | [#47](https://github.com/linagora/twalk/issues/47) | specced — tickets #55–#57 |
| Companion (PWA) | v0.1 | [#65](https://github.com/linagora/twalk/issues/65) | specced — tickets #66–#70 |
| Buzz Control Room integration | v0.1 | — | planned |
| Sovereign SMS (Twake SMS Companion) | v0.2 | — | planned |
| Telegram and Discord onboarding | v0.2 | — | planned |
| Personas `watch`, `archive`, `writing`, triage | v0.2 | — | planned |
| Persona SDK (Python, TypeScript) | v0.2 | — | planned |
| Kubernetes overlays | v0.2 | — | planned |
| Contract freeze and public schemas | v1.0 | — | planned |
| Consent policies, audit export, guided recovery | v1.0 | — | planned |

Only two lots are specced today: the Sensor and Hermes. Every lot marked *planned* needs its spec issue written before any ticket can be picked up — that is the deliberate gate, not an oversight.

---

## v0.1 — end-to-end path (target: end of 2026)

The milestone: four networks live (WhatsApp, Signal, SMS through mautrix-gmessages as an explicit proof of concept, and an existing Matrix account), Sensor stable, the `assistant` persona in Hermes, Buzz Control Room for oversight, deployable with Docker Compose, and a Companion that walks a non-technical user from account bootstrap to an active persona.

### Contract — CloudEvents v1 · done

The 8 event types in [`contracts/cloudevents/v1/`](../../contracts/cloudevents/v1/) with one validated fixture each, plus the `network`, `consent` and `traceparent` extensions ([ADR 0007](adr/0007-cloudevents-envelope-conventions.md)). It is the source of truth every other lot codes against, which is why it landed first. The contract stays open to additions until v1.0 freezes it.

### Sensor · done

Spec [#1](https://github.com/linagora/twalk/issues/1), tickets 01–11, all merged. The inbound chokepoint: joins portal rooms, decrypts, enriches with contact and consent context, publishes schema-valid CloudEvents exactly once per occurrence, and posts approved replies back into portal rooms. A code review of the whole lot on 2026-09-17 produced six bug tickets ([#26](https://github.com/linagora/twalk/issues/26)–[#31](https://github.com/linagora/twalk/issues/31)), all fixed and merged the same day.

Since then the Sensor also knows who its operator is: [#109](https://github.com/linagora/twalk/issues/109) gave the user's own messages a type of their own, `outbound.message.sent`, with the operator's Matrix ID as the subject and no `consent` extension at all ([ADR 0018](adr/0018-the-users-own-messages-are-their-own-event-type.md)) — before it, a portal room's other direction went out as `inbound.message.received` with the user labelled `pending`, and the full-loop ticket [#25](https://github.com/linagora/twalk/issues/25) would have demonstrated a persona answering its own operator. Which network ghosts are the operator's is handed to the Sensor by the deployment (`SENSOR_OWNER`, `SENSOR_OWNER_IDENTITIES`) and is not resolvable from the bridges today; see the ticket. [#147](https://github.com/linagora/twalk/issues/147) finished the job for the rest of the traffic ([ADR 0021](adr/0021-the-owner-is-never-a-contact-on-any-event.md)): the operator's own reaction is `outbound.reaction.added`, their own presence is not published at all, and a test reads the contract directory so a new type cannot be added without answering what it does about the owner. It was not cosmetic — a network-wide grant, which is how a user with hundreds of contacts actually decides, labelled the operator's own reaction `granted` and attached the phone number their ghost localpart is minted from.

Known follow-ups, none blocking: [#13](https://github.com/linagora/twalk/issues/13) (reaction excerpts in encrypted rooms), [#38](https://github.com/linagora/twalk/issues/38) (test isolation). [#16](https://github.com/linagora/twalk/issues/16) (the consent cache restarts cold) was the seventh, and it is closed by the Gateway lot's [#51](https://github.com/linagora/twalk/issues/51): the Sensor reads the Gateway's consent snapshot at startup and follows the bus from the sequence it names.

### Matrix as a network · in progress

[ADR 0009](adr/0009-matrix-is-a-network.md) settled that a native Matrix account is a network like any other, not a special case. Two tickets carry it: [#17](https://github.com/linagora/twalk/issues/17) extends the contract (`network=matrix`, encryption material on attachments), then [#18](https://github.com/linagora/twalk/issues/18) implements the Sensor side. This is what makes the v0.1 "bring your own Matrix account" path real.

### Hermes · in progress

Spec [#19](https://github.com/linagora/twalk/issues/19), tickets H1–H6, mostly sequential: **H1** [#20](https://github.com/linagora/twalk/issues/20) shared test harness crate and stub LLM → **H2** [#21](https://github.com/linagora/twalk/issues/21) Python persona SDK and `assistant` skeleton → **H3** [#22](https://github.com/linagora/twalk/issues/22) suggestion production and **H4** [#23](https://github.com/linagora/twalk/issues/23) runtime lifecycle (parallel) → **H5** [#24](https://github.com/linagora/twalk/issues/24) approval API → **H6** [#25](https://github.com/linagora/twalk/issues/25) the full loop end-to-end.

The runtime is Rust, personas are separate processes talking to the bus, and the reference persona `assistant` is Python against the SDK ([ADR 0008](adr/0008-hermes-rust-runtime-personas-as-processes.md)).

Two gates live in the SDK rather than in a persona, for the same reason — an author cannot forget them: the **consent gate**, and since [#109](https://github.com/linagora/twalk/issues/109) the **trigger-type gate**, which refuses to wake a persona on anything but an inbound message. The second exists because the consent gate structurally cannot stop the user's own messages: `outbound.message.sent` carries no consent extension to read ([ADR 0018](adr/0018-the-users-own-messages-are-their-own-event-type.md)).

A design review on 2026-09-18 revisited only what had changed since that spec was written, and produced four decisions with consequences outside the H-series:

- **Human approval has a surface.** Spec #19 called the Companion Gateway the approval API's "future caller"; it exists, so the Gateway relays approvals and reads suggestions from the bus without persisting them ([#97](https://github.com/linagora/twalk/issues/97)), and the Companion carries an approval screen separate from the dashboard ([#100](https://github.com/linagora/twalk/issues/100)). H5 also re-checks the contact's **current** consent before publishing, which closes a gap between two earlier decisions: a revocation after a suggestion was produced previously left the approval passing.
- **No default model** ([ADR 0015](adr/0015-no-default-llm-configured-through-the-companion.md)): a persona refuses to start without an endpoint the operator chose, the configuration is held by the Gateway and injected by the runtime, and an operator-supplied credential file wins over one set from the browser.
- **A suggestion follows the conversation's language, not the user's** ([ADR 0016](adr/0016-a-reply-follows-the-conversation-not-the-user.md)) — the user's native language is a stored preference governing the interface and the fallback. The Companion ships five languages, three of them not yet reviewed by native speakers, with a contribution path in `CONTRIBUTING.md`.
- **Agent observability is OpenTelemetry, off by default** ([ADR 0017](adr/0017-agent-observability-is-otlp-and-opt-in.md)): the bus says what a persona did, traces say why it proposed that, and prompt content is a second, separately named decision because those prompts are the user's messages.

### Bridges · in progress

Configurations and appservice registrations for mautrix-whatsapp, mautrix-signal and mautrix-gmessages, wired into the reference deployment so a Sensor sees real portal rooms. WhatsApp and Signal landed with [#73](https://github.com/linagora/twalk/issues/73): pinned images behind the `bridges` compose profile, registrations generated at provisioning time, and a deployment test that proves the stack without ever attempting a login — pairing a phone is a human act. mautrix-gmessages follows. Twalk maintains no forks: improvements go upstream, configurations come back as documentation or fixtures. The v0.1 SMS path through mautrix-gmessages is knowingly non-sovereign (Google account cookie) and is v0.2's debt to pay.

### Companion Gateway · specced

The Companion's backend, in Rust: bridge provisioning facade, persona orchestrator, and **sole writer of consent state** ([ADR 0006](adr/0006-consent-state-owned-by-companion-gateway.md)). It produces three of the ten event types — `consent.state.changed.v1`, `bridge.status.changed.v1` and, since [#24](https://github.com/linagora/twalk/issues/24), `persona.reply.approved.v1` ([ADR 0022](adr/0022-the-approval-api-lives-on-the-companion-gateway.md): the approval is refused unless the sender's consent is still granted at that moment, and the single writer of consent state should not have to ask another service what it wrote) — and it owns the consent snapshot the Sensor needs to label events correctly after a restart, which is the real fix for [#16](https://github.com/linagora/twalk/issues/16).

It blocks the Companion lot: the PWA is a client of this API and has nothing to call without it.

Spec [#46](https://github.com/linagora/twalk/issues/46), tickets #48–#54, mostly sequential: **G1** service skeleton with the Sensor's operational parity → **G2** consent store, journal and outbox → **G3** snapshot endpoint naming its stream position → **G4** the Sensor reading it, which closes [#16](https://github.com/linagora/twalk/issues/16); **G5** sign-in and per-device tokens → **G6** registration relay and Sensor invitation; **G7** the pending-contact projection.

The design decisions behind it: [ADR 0010](adr/0010-consent-snapshot-then-deltas.md) (snapshot then deltas, arbitrated by the stream sequence), [ADR 0011](adr/0011-gateway-authenticates-with-matrix-openid.md) (Matrix OpenID, no access token held, one owner per deployment), [ADR 0013](adr/0013-persona-activation-is-a-consent-decision.md) (persona activation is a consent decision) and [ADR 0022](adr/0022-the-approval-api-lives-on-the-companion-gateway.md) (the approval API is here rather than on the Hermes runtime, overriding spec [#19](https://github.com/linagora/twalk/issues/19)).

### Bridge provisioning facade · specced

Spec [#47](https://github.com/linagora/twalk/issues/47), tickets #55–#57: **B1** the login proxy, which holds mautrix's blocking QR step server-side and exposes a pollable state (a phone that sleeps mid-scan must not lose the login) → **B2** bridge status by webhook, reconciled at startup, the sole producer of `bridge.status.changed`; **B3** the SMS preview path and its Google cookie relay.

It stays a thin facade: no container control, and no appservice registration generation, because installing one requires a homeserver config edit and a restart. Its network side is never proven by the test suite — a real bridge needs a live WhatsApp or Signal account — which the spec states rather than hides.

### Companion (PWA) · specced

The user-facing configuration surface, a SvelteKit static export — the part of v0.1 a non-technical user actually touches. The screens are already designed and reviewed in [`docs/wireframes/companion-v0.1.md`](../wireframes/companion-v0.1.md), so the lot's spec starts from settled UX rather than a blank page:

1. **Bootstrap** — welcome and homeserver (screen 1), account creation and recovery key (screen 2).
2. **Channel onboarding** — picker (screen 3) plus one screen per network, because login mechanism, failure modes and trust story differ: WhatsApp QR (3a), Signal secondary-device QR (3b), SMS through Google Messages (3c), existing Matrix account (3d).
3. **Persona activation** — `assistant` with safe defaults: reading on, suggestions on, auto-send off (screen 4).
4. **Home dashboard** — system health and Messagr pairing (screen 5).

Explicitly out of v0.1: Telegram and Discord screens, multi-persona management, the searchable consent inbox, consent policies with time windows, bridge diagnostic deep-dive, product tour, native mobile shell. Design tokens follow the Messagr design system; screens are mobile-first (375–428 px), accessible and localized.

Spec [#65](https://github.com/linagora/twalk/issues/65), tickets #66–#70: **C1** the foundation (static export, design tokens, generated API client, capability gate) → **C2** the bootstrap journey, **C3** the networks journey, **C4** persona activation and the dashboard, and **C5** a verification pass on a real iPhone.

[ADR 0014](adr/0014-companion-crypto-runs-in-the-browser.md) settles the cryptographic shape: the recovery key is generated in the browser because it cannot be generated anywhere else — matrix-js-sdk removed its non-WebAssembly backend — and the crypto store is unencrypted, losable and recoverable, with iOS's seven-day eviction of a Safari tab treated as a normal event rather than an error. C5 exists because none of that is verifiable by automation.

Four decisions of that review overrule the reviewed wireframes (no auto-send toggle, no active hours, no address-book consent default, an operational-only activity feed); [#74](https://github.com/linagora/twalk/issues/74) corrects the design document so the change is visible rather than silent.

### Buzz Control Room integration · planned

Oversight for what the personas do: suggestions surfaced for approval, approvals flowing back on the bus. Buzz itself is built outside this repo; this lot is the integration — the assets in [`ui/`](../../ui/), the event-level contract it consumes, and its place in the reference deployment. Hermes H5 (approval API) is the natural dependency.

---

## v0.2 — sovereignty and breadth (target: Q1 2027)

- **Sovereign SMS.** The first-party Twake SMS Companion Android app on F-Droid (ADR 0004, referenced across the docs but not yet written) replaces mautrix-gmessages as the reference SMS path (design retained as screen 3c-next). mautrix-gmessages stays supported for operators who prefer it. This pays down the v0.1 proof-of-concept debt.
- **Telegram and Discord onboarding.** mautrix-telegram and mautrix-discord, with their Companion screens.
- **Four more personas.** `watch`, `archive`, `writing`, and a triage persona, on the Hermes runtime v0.1 proved.
- **Persona SDK.** Python and TypeScript packages for third-party authors (Rust later), with the authoring guide.
- **Kubernetes overlays.** Alongside the Compose reference; bare-metal Ansible later.
- **Companion at full breadth.** All six networks, per-contact consent decisions, Messagr pairing by QR code.

## v1.0 — a contract others can build on (target: Q2 2027)

- **Contract freeze.** The 8 v1 types frozen, JSON Schemas published at a stable URL, a hosted validator for third-party persona authors.
- **Companion.** Consent policies with time windows, audit log export, guided bridge recovery flows.
- **Erasing history.** Deleting a contact's past events, an explicit action distinct from revoking consent ([ADR 0012](adr/0012-revoked-consent-reduces-publication.md)), alongside the audit export and the import that has to come with it.

---

## Build order

```
Contract v1 ─┬─▶ Sensor ──────────────┬─▶ Matrix as a network (#17 done ─▶ #18)
             │                        │
             ├─▶ Hermes H1 ─▶ H2 ─┬─▶ H3 ─────────────┐
             │                    └─▶ H4 ─▶ H5 ─▶ H6 ─┴─▶ Buzz Control Room
             │
             └─▶ Gateway G1 ─┬─▶ G2 ─┬─▶ G3 ─▶ G4 (closes #16)
                             │       └─▶ G7
                             └─▶ G5 ─┬─▶ G6
                                     ├─▶ Companion (PWA)
                                     └─▶ Bridge facade B1 ─┬─▶ B2
                                                           └─▶ B3

Bridges ─────▶ (independent; needed for a real end-to-end v0.1 demo)
```

What this says in practice: the contract gated everything and is done; the Sensor gated the inbound path and is done; Hermes is the current critical path to a working persona loop; the Companion Gateway is the critical path to a user-installable v0.1, and since 2026-09-17 it is specced and its first ticket is startable. Bridges and the Gateway can both start in parallel with Hermes — they share no files with it.

Three tickets sit outside every lot: [#58](https://github.com/linagora/twalk/issues/58) (a revoked sender's message is published without its body — [ADR 0012](adr/0012-revoked-consent-reduces-publication.md), a contract change to make before the v1.0 freeze), [#59](https://github.com/linagora/twalk/issues/59) (write the security model the README already links) and [#60](https://github.com/linagora/twalk/issues/60) (the Hermes runtime gates each persona on its own consent state, the counterpart of ADR 0013).

## Keeping this document honest

Update it when a lot changes state, when a lot is specced (link the spec issue), or when a milestone's scope moves in the README. Status detail belongs in the tracker; this file holds the mapping and the order.

Referenced but not yet written, so nobody hunts for them: ADRs 0001–0004, `docs/architecture/overview.md`, `docs/guides/persona-authoring.md`. The security model is written: [`security-model.md`](security-model.md).
