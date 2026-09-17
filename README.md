# Twalk

**A sovereign, open source event hub for personal multi-channel messaging.**

Twalk turns your fragmented messaging landscape (WhatsApp, Telegram, Signal, Discord, SMS/RCS, and eventually email) into a single, auditable stream of typed events that agents can observe, reason about, and respond to under human control.

Unlike push notification services or unified inbox products, Twalk is not a SaaS. It is an infrastructure you self-host, whose contract with the outside world is a versioned CloudEvents 1.0 schema. Everything is open source, replayable and portable.

> [!NOTE]
> Twalk is developed by [LINAGORA](https://linagora.com) as part of the Twake, Twaky, Messagr and Buzz ecosystem. The name Twalk is a contraction of *Twake* and *Talk*: the place where every Twake conversation converges, whatever the underlying channel.

---

## What Twalk does

Twalk sits between the messaging networks you use every day and the agents or interfaces that act on your behalf. It provides six clearly separated planes:

1. **Collection.** Mautrix bridges connect to WhatsApp, Telegram, Signal, Discord and SMS/RCS. Each conversation lands in an encrypted Matrix portal room.
2. **Normalization.** A Matrix client called Sensor decrypts events end-to-end, enriches them with contact and channel context, and publishes them as typed CloudEvents.
3. **Bus.** NATS JetStream stores the event stream durably, with replay, filtering and at-least-once delivery.
4. **Agents.** Hermes hosts agentic personas that consume events, reason with a local or remote LLM, and produce suggestions or replies on the same bus.
5. **Interfaces.** Messagr provides personal messaging, Buzz provides agent oversight through Control Room, Watch Room and Dialogue Room patterns.
6. **Configuration.** Twalk Companion, a self-hosted Progressive Web Application, guides a non-technical user from Matrix account bootstrap to bridge login, persona activation and consent management. Backed by the Companion Gateway service.

The whole system is designed so that no message ever leaves your infrastructure without your explicit consent, and so that any agent action can be reviewed, replayed or reversed from the event log.

---

## Why Twalk exists

Existing solutions in this space fall into three categories, none of which fit a sovereign, agent-ready world.

**Cloud aggregators** (Beeper, Texts.com) unify messaging into a hosted inbox. They solve the surface problem but require trusting a third party with the full content of your conversations, and they are not designed as event platforms for agents.

**Self-hosted Matrix stacks** (matrix-docker-ansible-deploy, Element Server Suite) solve hosting but leave you with a bag of loosely-coupled bridges, no unified event contract, and no obvious place to plug agents in.

**Point automation tools** (n8n, Zapier, Node-RED) offer flow-building but treat messages as opaque strings, without cryptographic identity, without conversation state, and without a replayable audit trail.

Twalk fills the gap: a Matrix-native event backbone, a stable CloudEvents contract, a persona-driven agent runtime and a set of oversight interfaces, all sovereign and self-hostable.

---

## Architecture at a glance

```
┌──────────────┐   ┌───────────────────┐   ┌─────────────────────┐   ┌──────────────┐   ┌────────────────────┐
│   Networks   │   │ Bridges &         │   │ Matrix Hub          │   │ Event Bus    │   │ Agents & Interfaces│
│ WhatsApp     │──▶│ Companions        │──▶│ Synapse             │──▶│ NATS         │──▶│ Hermes personas    │
│ Telegram     │   │ mautrix-whatsapp  │   │  + portal rooms     │   │ JetStream    │   │ Messagr messaging  │
│ Signal       │   │ mautrix-telegram  │   │ Sensor              │   │              │◀──│ Buzz oversight     │
│ Discord      │   │ mautrix-signal    │   │  decrypt/enrich     │   │ CloudEvents  │   │                    │
│ SMS          │   │ mautrix-discord   │   │  publish events     │   │ v1 topics    │   │                    │
│              │◀──│ mautrix-gmessages │◀──│                     │   │              │   │                    │
│              │   │  (v0.1, PoC)      │   │                     │   │              │   │                    │
│              │   │ Twake SMS         │   │                     │   │              │   │                    │
│              │   │  Companion (v0.2) │   │                     │   │              │   │                    │
└──────────────┘   └───────────────────┘   └─────────────────────┘   └──────────────┘   └────────────────────┘
       inbound path ▶                                                   ◀ outbound path
```

Left to right for inbound traffic (a contact sends you a WhatsApp message that reaches your agents), right to left for outbound (an agent-approved reply flows back to WhatsApp through the same bridge).

Full diagrams and event sequences: [`docs/architecture/overview.md`](docs/architecture/overview.md).

---

## Event contract

Every event on the bus is a valid CloudEvents 1.0 envelope. The `type` attribute follows a versioned reverse-DNS convention:

```
fr.linagora.twalk.<domain>.<action>.<version>
```

Eight event types are defined in the v1 contract:

| Type                                                     | Producer          | Purpose                                                                |
| -------------------------------------------------------- | ----------------- | ---------------------------------------------------------------------- |
| `fr.linagora.twalk.inbound.message.received.v1`          | Sensor            | A message from an external network was decrypted.                      |
| `fr.linagora.twalk.inbound.reaction.added.v1`            | Sensor            | A reaction was added to a message.                                     |
| `fr.linagora.twalk.inbound.presence.updated.v1`          | Sensor            | A contact came online or offline.                                      |
| `fr.linagora.twalk.persona.thinking.emitted.v1`          | Hermes            | A persona has started processing an event.                             |
| `fr.linagora.twalk.persona.suggest.produced.v1`          | Hermes            | A persona produced a suggested reply for oversight.                    |
| `fr.linagora.twalk.persona.reply.approved.v1`            | Hermes            | A suggestion was approved and should be sent.                          |
| `fr.linagora.twalk.consent.state.changed.v1`             | Companion Gateway | The user modified the consent state of a contact or a channel.         |
| `fr.linagora.twalk.bridge.status.changed.v1`             | Companion Gateway | A bridge changed state (connected, disconnected, session expired).     |

Three CloudEvents extensions are used consistently: `network` (source channel), `consent` (data-processing consent state), `traceparent` (W3C distributed tracing).

All schemas live in [`contracts/cloudevents/v1/`](contracts/cloudevents/v1/) and are the source of truth for every component.

---

## Repository layout

```
twalk/
├── contracts/           Source of truth: CloudEvents schemas, fixtures, persona spec
├── sensor/              The Sensor service (Rust): Matrix client, decrypts and publishes
├── hermes/              The agent platform: consumer runtime + reference personas
├── companion/           Twalk Companion PWA (SvelteKit static export): user-facing configuration surface
├── companion-gateway/   Companion backend (Rust): bridge provisioning facade, consent broker, persona orchestrator
├── bridges/             Mautrix bridge configurations and registrations
├── deploy/              Docker Compose, Kubernetes and Ansible deployment manifests
├── ui/                  Optional Buzz control room and admin console
├── sdk/                 Client libraries for third-party persona authors (Python, TS, Rust)
├── examples/            Runnable end-to-end scenarios
├── tools/               Operational scripts (replay, validation, schema generation)
├── tests/               Shared test harness crate (tests/harness/) and end-to-end tests
└── docs/                Architecture docs, ADRs, deployment and authoring guides
```

Each top-level directory has its own README with scope, boundaries and how to contribute.

---

## Quickstart

Requirements: Docker Engine 25+, Docker Compose v2.20+, a public DNS name for your Matrix homeserver, one phone or account per network you want to bridge.

```bash
git clone https://github.com/linagora/twalk.git
cd twalk/deploy/docker-compose
cp .env.example .env
# Edit .env: set MATRIX_DOMAIN, the Synapse secrets, the Sensor account credentials.
# SENSOR_ALLOWED_INVITERS must name the bridge bot accounts and your own account.
docker compose up -d
```

Once containers are healthy, the reference stack is running: Synapse (the Matrix hub), NATS JetStream (the bus), the Sensor, whose account the stack provisions itself on the way up, and the Companion Gateway, which serves the Companion's origin on `http://127.0.0.1:8080` (`GATEWAY_HTTP_PORT`) — a holding page until the Companion itself is built. From here, any allowed inviter — in production, a bridge's provisioning user — invites the Sensor into a portal room and its traffic lands as CloudEvents on the bus, with no further manual steps. Mautrix bridges register their own bot accounts through their appservice registration, so the allowed inviters exist without manual provisioning; `./provision.sh` remains for ad-hoc accounts. Bridges, Hermes personas and the oversight interfaces (Element Web, Buzz Control Room) join the reference stack as their components land; see the roadmap below.

The reference deployment lives in [`deploy/docker-compose/`](deploy/docker-compose/): its compose file and `.env.example` are the documented configuration surface. Kubernetes overlays and bare-metal Ansible land with later milestones.

---

## Roadmap

The v0.1 milestone (target: end of 2026) targets a functional end-to-end path: **four channels live** (WhatsApp via mautrix, Signal via mautrix, SMS via mautrix-gmessages **as an assumed proof-of-concept path**, plus an existing Matrix account bring-your-own), Sensor stable, one reference persona in Hermes (`assistant`), Buzz Control Room for validation, deployable via Docker Compose. Twalk Companion delivers Matrix account bootstrap or existing-account pairing, the four channel onboarding flows, and `assistant` persona activation on mobile web.

The v0.1 SMS path is explicitly a proof of concept: it uses [`mautrix-gmessages`](https://docs.mau.fi/bridges/go/gmessages/authentication.html) with its Google account cookie dependency. This dependency is transparent to the user (documented in the SMS onboarding flow) and treated as a v0.1 technical debt that v0.2 pays down.

The v0.2 milestone (target: Q1 2027) adds **Telegram and Discord onboarding**, and **replaces the mautrix-gmessages SMS path with the sovereign first-party [Twake SMS Companion](docs/architecture/adr/0004-twake-sms-companion-first-party-app.md) Android app published on F-Droid**. mautrix-gmessages remains a supported alternative for operators who prefer it, but is no longer the reference path. v0.2 also delivers the four other reference personas (`watch`, `archive`, `writing`, plus a triage persona), persona SDK in Python and TypeScript, and Kubernetes overlays. The Companion covers all six channels, per-contact consent decisions, and pairing with Messagr via QR code.

The v1.0 milestone (target: Q2 2027) freezes the CloudEvents v1 contract (8 types), publishes the JSON Schemas at a stable URL, and offers a hosted validator for third-party persona authors. The Companion adds consent policies with time windows, audit log export, and guided bridge recovery flows.

Detailed roadmap: [`docs/architecture/roadmap.md`](docs/architecture/roadmap.md).

---

## Ecosystem partnerships

Twalk is designed to compose with adjacent open source projects rather than reinvent them. When a mature building block already exists in the sovereign or open source ecosystem, our default strategy is **to adopt, contribute upstream, and preserve the interface**, not to fork or duplicate.

### Pimalaya Carillon (personal information management)

Twalk currently focuses on real-time messaging networks. As we extend the Sensor to email, calendaring, contacts, notes and RSS, we intend to **build on top of the [Pimalaya](https://pimalaya.org) sans-I/O Rust crates** rather than rewrite them. In particular:

- [`io-imap`](https://github.com/pimalaya/core), [`io-jmap`](https://github.com/pimalaya/core), [`io-webdav`](https://github.com/pimalaya/core), [`io-oauth`](https://github.com/pimalaya/core), [`io-maildir`](https://github.com/pimalaya/core) provide protocol logic decoupled from any concrete I/O runtime, dual-licensed MIT and Apache 2.0.
- The Pimalaya [Carillon](https://github.com/pimalaya/carillon) CLI already implements event-driven PIM watching (email, calendar, contacts, RSS) with a plugin architecture. When our roadmap reaches these channels, our first move will be to **integrate the Sensor as a downstream consumer of the same primitives**, and to contribute back the Matrix and CloudEvents bindings we develop for our own needs.

This means our upstream target is unambiguous: rather than compete on the personal information collection layer, Twalk aims to become the **event-bus and agentic runtime layer** that plugs into Pimalaya's collection layer, and to contribute Matrix, NATS and CloudEvents integrations where they make sense in Pimalaya's own roadmap.

We reserve the option to contribute a Matrix module or a CloudEvents emitter directly to the Pimalaya Carillon CLI itself when the design allows it.

### Matrix and Mautrix

Twalk is a first-class Matrix citizen. We do not maintain forks of Synapse, Element, or the Mautrix bridges maintained by Tulir Asokan and contributors. Any Matrix-side improvement Twalk depends on will be proposed as a pull request against the appropriate upstream project. Bridge configurations we develop for our own use are contributed back as documentation or fixtures.

### CloudEvents and NATS

Our event contract is a strict application of the [CloudEvents 1.0 specification](https://github.com/cloudevents/spec) maintained by the CNCF Serverless WG. We do not extend the specification; we use its extension mechanism when needed. Any tooling or schema fragment of general interest is offered to the CloudEvents ecosystem under CC0 1.0.

Our operational choices around [NATS JetStream](https://nats.io) are documented in [`docs/architecture/adr/0001-nats-jetstream-over-kafka.md`](docs/architecture/adr/0001-nats-jetstream-over-kafka.md) and can be reused by other projects facing similar constraints.

---

## Contributing

Twalk accepts contributions under the terms of its Developer Certificate of Origin. Before opening a pull request, please read:

- [`CONTRIBUTING.md`](CONTRIBUTING.md) for the development workflow.
- [`docs/architecture/adr/`](docs/architecture/adr/) for the reasoning behind structural choices.
- [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) for community expectations.

If you are building a persona for Hermes, start with [`docs/guides/persona-authoring.md`](docs/guides/persona-authoring.md) and the `twalk-sdk` package in your preferred language.

Contributions that improve interoperability with adjacent open source projects (Pimalaya, Matrix, Mautrix, CloudEvents tooling) are especially welcome and will be reviewed with priority.

---

## Security

If you find a security issue, please do not open a public issue. Report it privately following the process in [`SECURITY.md`](SECURITY.md). We aim to acknowledge within 72 hours and publish a coordinated fix within 30 days for severe issues.

Twalk's threat model, key handling and sovereignty guarantees are documented in [`docs/architecture/security-model.md`](docs/architecture/security-model.md).

---

## License

Twalk is released under the [GNU Affero General Public License v3.0](LICENSE). The CloudEvents schemas in `contracts/` are additionally released under [CC0 1.0](contracts/LICENSE) to encourage third-party interoperability.

---

## Naming and identity

Twalk (pronounced /twɔːk/) is a contraction of *Twake* and *Talk*. The name places the project inside the LINAGORA product family (Twake workplace, Twaky personal agent, Messagr messaging, Buzz oversight) and states its function transparently: the place where all conversations across all channels come together to be observed, reasoned about, and answered under human control.

The project was previously named Carillon during its early scoping phase. That name was retired in September 2026 to avoid confusion with the [Pimalaya Carillon](https://github.com/pimalaya/carillon) PIM watcher, which is an ecosystem partner rather than a competitor, and with adjacent commercial projects.

---

## Acknowledgments

Twalk builds on the shoulders of the Matrix community, the Mautrix bridge ecosystem maintained by Tulir Asokan and contributors, the Pimalaya sans-I/O Rust crates maintained by Clément Douin, the CloudEvents specification maintained by the CNCF Serverless WG, and NATS.io.
