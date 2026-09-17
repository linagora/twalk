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
| Hermes | v0.1 | [#19](https://github.com/linagora/twalk/issues/19) | in progress — H1 [#20](https://github.com/linagora/twalk/issues/20) started |
| Matrix as a network | v0.1 | — (standalone tickets) | in progress — contract [#17](https://github.com/linagora/twalk/issues/17), Sensor [#18](https://github.com/linagora/twalk/issues/18) |
| Bridges | v0.1 | — | planned |
| Companion Gateway | v0.1 | — | planned |
| Companion (PWA) | v0.1 | — (wireframes exist) | planned |
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

Known follow-ups, none blocking: [#16](https://github.com/linagora/twalk/issues/16) (the consent cache restarts cold — its real fix belongs to the Companion Gateway lot), [#13](https://github.com/linagora/twalk/issues/13) (reaction excerpts in encrypted rooms), [#38](https://github.com/linagora/twalk/issues/38) (test isolation).

### Matrix as a network · in progress

[ADR 0009](adr/0009-matrix-is-a-network.md) settled that a native Matrix account is a network like any other, not a special case. Two tickets carry it: [#17](https://github.com/linagora/twalk/issues/17) extends the contract (`network=matrix`, encryption material on attachments), then [#18](https://github.com/linagora/twalk/issues/18) implements the Sensor side. This is what makes the v0.1 "bring your own Matrix account" path real.

### Hermes · in progress

Spec [#19](https://github.com/linagora/twalk/issues/19), tickets H1–H6, mostly sequential: **H1** [#20](https://github.com/linagora/twalk/issues/20) shared test harness crate and stub LLM → **H2** [#21](https://github.com/linagora/twalk/issues/21) Python persona SDK and `assistant` skeleton → **H3** [#22](https://github.com/linagora/twalk/issues/22) suggestion production and **H4** [#23](https://github.com/linagora/twalk/issues/23) runtime lifecycle (parallel) → **H5** [#24](https://github.com/linagora/twalk/issues/24) approval API → **H6** [#25](https://github.com/linagora/twalk/issues/25) the full loop end-to-end.

The runtime is Rust, personas are separate processes talking to the bus, and the reference persona `assistant` is Python against the SDK ([ADR 0008](adr/0008-hermes-rust-runtime-personas-as-processes.md)).

### Bridges · planned

Configurations and appservice registrations for mautrix-whatsapp, mautrix-signal and mautrix-gmessages, wired into the reference deployment so a Sensor sees real portal rooms. Twalk maintains no forks: improvements go upstream, configurations come back as documentation or fixtures. The v0.1 SMS path through mautrix-gmessages is knowingly non-sovereign (Google account cookie) and is v0.2's debt to pay.

### Companion Gateway · planned

The Companion's backend, in Rust: bridge provisioning facade, persona orchestrator, and **sole writer of consent state** ([ADR 0006](adr/0006-consent-state-owned-by-companion-gateway.md)). It produces two of the eight event types — `consent.state.changed.v1` and `bridge.status.changed.v1` — and it owns the consent snapshot the Sensor needs to label events correctly after a restart, which is the real fix for [#16](https://github.com/linagora/twalk/issues/16).

It blocks the Companion lot: the PWA is a client of this API and has nothing to call without it.

### Companion (PWA) · planned

The user-facing configuration surface, a SvelteKit static export — the part of v0.1 a non-technical user actually touches. The screens are already designed and reviewed in [`docs/wireframes/companion-v0.1.md`](../wireframes/companion-v0.1.md), so the lot's spec starts from settled UX rather than a blank page:

1. **Bootstrap** — welcome and homeserver (screen 1), account creation and recovery key (screen 2).
2. **Channel onboarding** — picker (screen 3) plus one screen per network, because login mechanism, failure modes and trust story differ: WhatsApp QR (3a), Signal secondary-device QR (3b), SMS through Google Messages (3c), existing Matrix account (3d).
3. **Persona activation** — `assistant` with safe defaults: reading on, suggestions on, auto-send off (screen 4).
4. **Home dashboard** — system health and Messagr pairing (screen 5).

Explicitly out of v0.1: Telegram and Discord screens, multi-persona management, the searchable consent inbox, consent policies with time windows, bridge diagnostic deep-dive, product tour, native mobile shell. Design tokens follow the Messagr design system; screens are mobile-first (375–428 px), accessible and localized.

Wireframes are not a spec: the lot still needs its spec issue (API surface against the Companion Gateway, session and pairing model, offline behaviour, test seam).

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

---

## Build order

```
Contract v1 ─┬─▶ Sensor ──────────────┬─▶ Matrix as a network (#17 ─▶ #18)
             │                        │
             ├─▶ Hermes H1 ─▶ H2 ─┬─▶ H3 ─────────────┐
             │                    └─▶ H4 ─▶ H5 ─▶ H6 ─┴─▶ Buzz Control Room
             │
             └─▶ Companion Gateway ─▶ Companion (PWA)

Bridges ─────▶ (independent; needed for a real end-to-end v0.1 demo)
```

What this says in practice: the contract gated everything and is done; the Sensor gated the inbound path and is done; Hermes is the current critical path to a working persona loop; the Companion Gateway is the critical path to a user-installable v0.1 and is not started. Bridges and the Gateway can both start in parallel with Hermes — they share no files with it.

## Keeping this document honest

Update it when a lot changes state, when a lot is specced (link the spec issue), or when a milestone's scope moves in the README. Status detail belongs in the tracker; this file holds the mapping and the order.

Referenced but not yet written, so nobody hunts for them: ADRs 0001–0004, `docs/architecture/overview.md`, `docs/architecture/security-model.md`, `docs/guides/persona-authoring.md`.
