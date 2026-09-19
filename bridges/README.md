# bridges

Mautrix bridge configurations and registrations. Twalk does not maintain forks of the Mautrix bridges; improvements are proposed upstream, and configurations developed here are contributed back as documentation or fixtures.

A **bridge** connects one external messaging network to Matrix and lands each conversation in a portal room; it is identified by `bridge_id` and is never a `network` value (`CONTEXT.md`, [ADR 0005](../docs/architecture/adr/0005-network-channel-and-bridge.md)).

## What is here

| Path | What it is |
| --- | --- |
| `mautrix-whatsapp/config.yaml` | The WhatsApp bridge's base configuration: this deployment's decisions, no secrets |
| `mautrix-signal/config.yaml` | The same, for Signal |
| `mautrix-gmessages/config.yaml` | The same, for SMS through Google Messages — the v0.1 SMS path |
| `generate-registration.sh` | Renders a base config with the operator's environment and generates the appservice registration; runs inside the bridge's own image |

The reference deployment wires all four together — see [`../deploy/README.md`](../deploy/README.md) for the two-step procedure that brings a bridge up, and `../deploy/docker-compose/.env.example` for every variable.

Each directory is named for the bridge, not for the network. `mautrix-gmessages` serves the **`sms`** network, and `gmessages` is never a network value (ADR 0005) — the mapping from this bridge to that network lives in one place, `GATEWAY_BRIDGE_MAUTRIX_GMESSAGES_NETWORK` in `../deploy/docker-compose/compose.yaml`. Inside these files `gmessages` appears only where mautrix itself uses it: the binary, the appservice id, the bot's localpart, the ghost prefix and the registration's filename.

## Versions are pinned

`../deploy/docker-compose/compose.yaml` pins `dock.mau.dev/mautrix/whatsapp:v26.09`, `dock.mau.dev/mautrix/signal:v26.09` and `dock.mau.dev/mautrix/gmessages:v26.09`. All three are deliberate, not the result of a `:latest` that happened to be current.

A bridge holds the user's account on an external network: their WhatsApp session, their Signal identity, their Google session ([`../docs/architecture/security-model.md`](../docs/architecture/security-model.md)). A version that changes under a working login is a login that can break while the user is asleep, and an upstream schema migration is not reversible. Upgrading is an operator's decision, taken with the upstream changelog open.

The gmessages pin is the one with the least slack. Its login cannot be repaired by a script — see below — so an upgrade that invalidates the stored session costs a human a trip to a private browsing window, and mautrix documents no lifetime for that session in the first place.

The base configs are deliberately partial. Mautrix merges a partial config onto the example config of the running version, so every key these files do not mention keeps that version's upstream default. That keeps them reviewable, and keeps us from freezing defaults we have no opinion about.

## How a bridge is configured

Two files, and the split matters.

**The base config** (`mautrix-<id>/config.yaml`) is committed and holds no secret. It carries the decisions: SQLite in the bridge's own volume, the appservice listener on `0.0.0.0` so the compose network can reach it, encrypted portal rooms, provisioning authenticated by a shared secret and *not* by a Matrix access token, non-federated rooms, JSON logs to stdout, and `bridge_status_notices` left at its upstream default.

**The environment** (`.env`) supplies everything that depends on the deployment: the homeserver's domain and internal URL, the appservice's `as_token` and `hs_token`, the provisioning shared secret, the Companion Gateway's status-webhook URL, and the owner's Matrix ID.

`generate-registration.sh` puts the two together at provisioning time: it copies the base config into the bridge's data volume, renders the environment onto it with `yq`, runs `mautrix-<id> -g` — which is what actually builds a valid registration, with its id, url, namespaces and MSC2409 flags — and then pins the two tokens and the sender localpart back to the operator's values, because `-g` regenerates all three on every run. Which bridge it is running as comes from `BRIDGE_MAUTRIX_ID` (`whatsapp`, `signal`, `gmessages`): that variable selects the binary and the registration's filename, and it is deliberately not called a network. That pinning is what makes the script idempotent, and it is why `.env` stays the single source of both halves of the appservice's identity: the Companion Gateway needs the same `as_token` in its own configuration to verify this bridge's status webhook ([#56](https://github.com/linagora/twalk/issues/56)).

One consequence is worth stating before it surprises someone: `/data/config.yaml` inside a bridge's volume is **generated, not authoritative**. Every provisioning run overwrites it from the base config plus `.env`, so an edit made in place survives exactly until the next `./provision-bridges.sh`. Change the base config or `.env` instead — that is the whole point of the split.

The generated registration is world-readable inside its volume, because Synapse reads it as its own user and not as the bridge's. It holds both appservice tokens; the volume is reachable only by root on the host, and protecting that host is the operator's job.

## The two HTTP surfaces a bridge exposes

All of them live on the appservice listener (WhatsApp 29318, Signal 29328, Google Messages 29336 — each bridge's own upstream default), which the reference stack publishes on localhost only.

**Liveness**, unauthenticated: `GET /_matrix/mau/live` and `GET /_matrix/mau/ready`. The compose healthcheck probes `live`, deliberately: `ready` additionally waits for the *network* connection, which without a login never comes, so a bridge nobody has logged in to yet would never be healthy.

**Provisioning**, `GET|POST /_matrix/provision/v3/*` — `login/flows`, `login/start/{flow}`, `login/step/...`, `login/cancel`, `logout`, `whoami`. Authenticated by `Authorization: Bearer <provisioning.shared_secret>`. Three things are worth knowing before writing a client for it:

- The secret must be **at least 16 characters**, or the whole API answers `M_FORBIDDEN` without saying why. `generate-registration.sh` refuses a shorter one, where the message can be read.
- Shared-secret auth still needs **`?user_id=`**: the secret says who may call, the query parameter says whom the call is about, and mautrix checks that user against `bridge.permissions` — which this deployment sets to the single owner. Without it the answer is `M_FORBIDDEN, User does not have login permissions`.
- `GET /_matrix/provision/v3/whoami` is the cheap state probe: it names the network, the login flows, the bridge bot, and the logins that exist (`[]` until a human has paired a phone).

Status goes the other way. `homeserver.status_endpoint` points at the Companion Gateway's webhook, and the bridge POSTs its connection state there authenticated with its own `as_token` — mautrix's only push channel, and the producer behind `bridge.status.changed` on the bus.

## Logging in is a human act

WhatsApp and Signal are paired by scanning a QR code with the phone that holds the account.

SMS through Google Messages is worse, and the honesty about it is part of the feature. Google switched the QR sign-in off for third-party clients in 2024, so this bridge signs in with **seven Google session cookies** — `SID HSID OSID SSID APISID SAPISID` required, `__Secure-1PSIDTS` optional — taken from a **private** browsing window of the user's own Google account, followed by an emoji match on their phone. A private window is required because a normal one rotates the cookies and signing out of it invalidates the session the bridge holds, and Chrome's Device Bound Session Credentials must be off because they bind the session to that machine's hardware key.

Nothing in this repository asks for those cookies, reads them, stores them or automates their extraction. The Companion's screen 3c takes them from the user, the Companion Gateway relays them to the bridge and keeps none ([#57](https://github.com/linagora/twalk/issues/57), ADR 0011). Driving a headless browser to harvest them was considered and **rejected**: it is fragile, and it would make an automated login look like a human session to Google. That decision stands.

Mautrix documents no lifetime for those cookies, so nothing here promises a re-login cadence. When the session dies, the login is repaired from the Companion with the same flow against the same login id.

And the framing this path ships with does not get dropped quietly: SMS transits Google Messages Web, it needs a Google account, it is unavailable to an iOS-only user, and v0.2 replaces it with the sovereign first-party Twake SMS Companion. `sms` stays the network throughout that migration; only the `bridge_id` changes.

No automation replaces any of this, which is why `sensor/tests/bridges_deployment.rs` proves the deployment — Synapse accepts each appservice, each bridge process is live, each provisioning API answers `whoami` with `logins: []` — and never attempts a login.
