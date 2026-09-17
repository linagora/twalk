# bridges

Mautrix bridge configurations and registrations. Twalk does not maintain forks of the Mautrix bridges; improvements are proposed upstream, and configurations developed here are contributed back as documentation or fixtures.

A **bridge** connects one external messaging network to Matrix and lands each conversation in a portal room; it is identified by `bridge_id` and is never a `network` value (`CONTEXT.md`, [ADR 0005](../docs/architecture/adr/0005-network-channel-and-bridge.md)).

## What is here

| Path | What it is |
| --- | --- |
| `mautrix-whatsapp/config.yaml` | The WhatsApp bridge's base configuration: this deployment's decisions, no secrets |
| `mautrix-signal/config.yaml` | The same, for Signal |
| `generate-registration.sh` | Renders a base config with the operator's environment and generates the appservice registration; runs inside the bridge's own image |

The reference deployment wires all three together — see [`../deploy/README.md`](../deploy/README.md) for the two-step procedure that brings a bridge up, and `../deploy/docker-compose/.env.example` for every variable.

## Versions are pinned

`../deploy/docker-compose/compose.yaml` pins `dock.mau.dev/mautrix/whatsapp:v26.09` and `dock.mau.dev/mautrix/signal:v26.09`. Both are deliberate, not the result of a `:latest` that happened to be current.

A bridge holds the user's account on an external network: their WhatsApp session, their Signal identity ([`../docs/architecture/security-model.md`](../docs/architecture/security-model.md)). A version that changes under a working login is a login that can break while the user is asleep, and an upstream schema migration is not reversible. Upgrading is an operator's decision, taken with the upstream changelog open.

The base configs are deliberately partial. Mautrix merges a partial config onto the example config of the running version, so every key these files do not mention keeps that version's upstream default. That keeps them reviewable, and keeps us from freezing defaults we have no opinion about.

## How a bridge is configured

Two files, and the split matters.

**The base config** (`mautrix-<network>/config.yaml`) is committed and holds no secret. It carries the decisions: SQLite in the bridge's own volume, the appservice listener on `0.0.0.0` so the compose network can reach it, encrypted portal rooms, provisioning authenticated by a shared secret and *not* by a Matrix access token, non-federated rooms, JSON logs to stdout, and `bridge_status_notices` left at its upstream default.

**The environment** (`.env`) supplies everything that depends on the deployment: the homeserver's domain and internal URL, the appservice's `as_token` and `hs_token`, the provisioning shared secret, the Companion Gateway's status-webhook URL, and the owner's Matrix ID.

`generate-registration.sh` puts the two together at provisioning time: it copies the base config into the bridge's data volume, renders the environment onto it with `yq`, runs `mautrix-<network> -g` — which is what actually builds a valid registration, with its id, url, namespaces and MSC2409 flags — and then pins the two tokens and the sender localpart back to the operator's values, because `-g` regenerates all three on every run. That pinning is what makes the script idempotent, and it is why `.env` stays the single source of both halves of the appservice's identity: the Companion Gateway needs the same `as_token` in its own configuration to verify this bridge's status webhook ([#56](https://github.com/linagora/twalk/issues/56)).

One consequence is worth stating before it surprises someone: `/data/config.yaml` inside a bridge's volume is **generated, not authoritative**. Every provisioning run overwrites it from the base config plus `.env`, so an edit made in place survives exactly until the next `./provision-bridges.sh`. Change the base config or `.env` instead — that is the whole point of the split.

The generated registration is world-readable inside its volume, because Synapse reads it as its own user and not as the bridge's. It holds both appservice tokens; the volume is reachable only by root on the host, and protecting that host is the operator's job.

## The two HTTP surfaces a bridge exposes

Both live on the appservice listener (WhatsApp 29318, Signal 29328), which the reference stack publishes on localhost only.

**Liveness**, unauthenticated: `GET /_matrix/mau/live` and `GET /_matrix/mau/ready`. The compose healthcheck probes `live`, deliberately: `ready` additionally waits for the *network* connection, which without a login never comes, so a bridge nobody has logged in to yet would never be healthy.

**Provisioning**, `GET|POST /_matrix/provision/v3/*` — `login/flows`, `login/start/{flow}`, `login/step/...`, `login/cancel`, `logout`, `whoami`. Authenticated by `Authorization: Bearer <provisioning.shared_secret>`. Three things are worth knowing before writing a client for it:

- The secret must be **at least 16 characters**, or the whole API answers `M_FORBIDDEN` without saying why. `generate-registration.sh` refuses a shorter one, where the message can be read.
- Shared-secret auth still needs **`?user_id=`**: the secret says who may call, the query parameter says whom the call is about, and mautrix checks that user against `bridge.permissions` — which this deployment sets to the single owner. Without it the answer is `M_FORBIDDEN, User does not have login permissions`.
- `GET /_matrix/provision/v3/whoami` is the cheap state probe: it names the network, the login flows, the bridge bot, and the logins that exist (`[]` until a human has paired a phone).

Status goes the other way. `homeserver.status_endpoint` points at the Companion Gateway's webhook, and the bridge POSTs its connection state there authenticated with its own `as_token` — mautrix's only push channel, and the producer behind `bridge.status.changed` on the bus.

## Logging in is a human act

Both networks are paired by scanning a QR code with the phone that holds the account. No automation replaces that, which is why `sensor/tests/bridges_deployment.rs` proves the deployment — Synapse accepts each appservice, each bridge process is live, each provisioning API answers `whoami` — and never attempts a login.
