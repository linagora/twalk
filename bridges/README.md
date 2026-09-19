# bridges

Mautrix bridge configurations and registrations. Twalk does not maintain forks of the Mautrix bridges; improvements are proposed upstream, and configurations developed here are contributed back as documentation or fixtures.

A **bridge** connects one external messaging network to Matrix and lands each conversation in a portal room; it is identified by `bridge_id` and is never a `network` value (`CONTEXT.md`, [ADR 0005](../docs/architecture/adr/0005-network-channel-and-bridge.md)).

## What is here

| Path | What it is |
| --- | --- |
| `mautrix-whatsapp/config.yaml` | The WhatsApp bridge's base configuration: this deployment's decisions, no secrets |
| `mautrix-signal/config.yaml` | The same, for Signal |
| `mautrix-gmessages/config.yaml` | The same, for SMS through Google Messages — the v0.1 SMS path |
| `mautrix-telegram/config.yaml` | The same, for Telegram — the one bridge that needs a credential from the network it bridges |
| `generate-registration.sh` | Renders a base config with the operator's environment and generates the appservice registration; runs inside the bridge's own image |

The reference deployment wires all five together — see [`../deploy/README.md`](../deploy/README.md) for the two-step procedure that brings a bridge up, and `../deploy/docker-compose/.env.example` for every variable.

Each directory is named for the bridge, not for the network. `mautrix-gmessages` serves the **`sms`** network, and `gmessages` is never a network value (ADR 0005) — the mapping from this bridge to that network lives in one place, `GATEWAY_BRIDGE_MAUTRIX_GMESSAGES_NETWORK` in `../deploy/docker-compose/compose.yaml`. Inside these files `gmessages` appears only where mautrix itself uses it: the binary, the appservice id, the bot's localpart, the ghost prefix and the registration's filename.

`mautrix-telegram` serves **`telegram`**, where the bridge id and the network value happen to be the same word. That is a coincidence, not a rule, and nothing derives one from the other: `GATEWAY_BRIDGE_MAUTRIX_TELEGRAM_NETWORK` still states it, in the same one place, because the next bridge for which they differ must not be a special case.

## Versions are pinned

`../deploy/docker-compose/compose.yaml` pins `dock.mau.dev/mautrix/whatsapp:v26.09`, `dock.mau.dev/mautrix/signal:v26.09`, `dock.mau.dev/mautrix/gmessages:v26.09` and `dock.mau.dev/mautrix/telegram:v26.09`. All four are deliberate, not the result of a `:latest` that happened to be current.

A bridge holds the user's account on an external network: their WhatsApp session, their Signal identity, their Google session, their Telegram account session ([`../docs/architecture/security-model.md`](../docs/architecture/security-model.md)). A version that changes under a working login is a login that can break while the user is asleep, and an upstream schema migration is not reversible. Upgrading is an operator's decision, taken with the upstream changelog open.

The gmessages pin is the one with the least slack. Its login cannot be repaired by a script — see below — so an upgrade that invalidates the stored session costs a human a trip to a private browsing window, and mautrix documents no lifetime for that session in the first place.

The base configs are deliberately partial. Mautrix merges a partial config onto the example config of the running version, so every key these files do not mention keeps that version's upstream default. That keeps them reviewable, and keeps us from freezing defaults we have no opinion about.

## How a bridge is configured

Two files, and the split matters.

**The base config** (`mautrix-<id>/config.yaml`) is committed and holds no secret. It carries the decisions: SQLite in the bridge's own volume, the appservice listener on `0.0.0.0` so the compose network can reach it, encrypted portal rooms, provisioning authenticated by a shared secret and *not* by a Matrix access token, non-federated rooms, JSON logs to stdout, and `bridge_status_notices` left at its upstream default.

**The environment** (`.env`) supplies everything that depends on the deployment: the homeserver's domain and internal URL, the appservice's `as_token` and `hs_token`, the provisioning shared secret, the Companion Gateway's status-webhook URL, and the owner's Matrix ID.

Telegram adds one kind of value to that list that no other bridge has: `network.api_id` and `network.api_hash`, an application the **operator** registers at <https://my.telegram.org/apps> against their own phone number. Every other credential above is a Matrix credential. This one belongs to the external network, it is required before a user can start a login at all, and there is no value this repository may ship — Telegram answers `API_ID_PUBLISHED_FLOOD` to an `api_id` that has been published or shared, so an empty value is safer than a plausible one. `generate-registration.sh` requires it only for that bridge, refuses mautrix's sample pair by value, refuses a non-numeric `api_id`, and says where to get one. It renders `api_id` as a *number*: mautrix's config upgrader copies that key with an integer type and silently skips a node of any other type, so a quoted value would leave upstream's sample in place rather than failing.

`generate-registration.sh` puts the two together at provisioning time: it copies the base config into the bridge's data volume, renders the environment onto it with `yq`, runs `mautrix-<id> -g` — which is what actually builds a valid registration, with its id, url, namespaces and MSC2409 flags — and then pins the two tokens and the sender localpart back to the operator's values, because `-g` regenerates all three on every run. Which bridge it is running as comes from `BRIDGE_MAUTRIX_ID` (`whatsapp`, `signal`, `gmessages`, `telegram`): that variable selects the binary and the registration's filename, and it is deliberately not called a network. That pinning is what makes the script idempotent, and it is why `.env` stays the single source of both halves of the appservice's identity: the Companion Gateway needs the same `as_token` in its own configuration to verify this bridge's status webhook ([#56](https://github.com/linagora/twalk/issues/56)).

One consequence is worth stating before it surprises someone: `/data/config.yaml` inside a bridge's volume is **generated, not authoritative**. Every provisioning run overwrites it from the base config plus `.env`, so an edit made in place survives exactly until the next `./provision-bridges.sh`. Change the base config or `.env` instead — that is the whole point of the split.

The generated registration is world-readable inside its volume, because Synapse reads it as its own user and not as the bridge's. It holds both appservice tokens; the volume is reachable only by root on the host, and protecting that host is the operator's job.

## What the Telegram config decides, and why it is not upstream's defaults

Three keys in `mautrix-telegram/config.yaml` are pinned at values a reader could mistake for inherited defaults. They are decisions, and the reason each is written down is that an upstream change to any of them would move something a deployment relies on without anyone choosing it.

**The register arrives populated.** Unlike WhatsApp, which builds portals purely lazily as each conversation becomes active, mautrix-telegram syncs the chat list at login: `network.sync.create_limit: 15`, `login_sync_limit: 15`, `direct_chats: true` mean roughly fifteen portal rooms appear at the instant of login. The portal register ([ADR 0024](../docs/architecture/adr/0024-a-portal-room-is-observed-by-invitation-per-conversation.md)) and the conversation chooser ([#143](https://github.com/linagora/twalk/issues/143)) therefore meet a list that arrives rather than one that grows from zero. Fifteen is kept because it is a list a human can review in one sitting and the same order of magnitude as the eighteen WhatsApp portals the reference deployment accumulated over a working day; `0` would be the deafness [#105](https://github.com/linagora/twalk/issues/105) was about, and `-1` a chooser nobody reads. Nothing is observed until the user chooses it, so what appears is a list of decisions waiting.

**A conversation's identity is not its Matrix room id.** `network.always_tombstone_on_supergroup_migration: false` is upstream's default and is pinned. Set it to `true` and a Telegram group promoted to a supergroup is replaced by a *new* Matrix room, tombstoning the old one — and the portal register is keyed on room id, as is the Sensor's own membership, which *is* the register's answer to "is this conversation observed". A conversation the user chose to observe would become a room the Sensor is not in, silently, after they decided about it. `false` migrates the room in place and keeps that decision meaningful. The underlying fact does not depend on the flag, which is the strongest argument for the `network_conversation_id` #143 threads through.

**A community-as-space would be offered as a conversation.** `network.bridge_communities: false` is the one departure from upstream here. A bridged community is itself a bridgev2 portal, so it carries the `m.bridge` state event the register uses to recognise a conversation, and it would be listed beside real conversations with a member count that means nothing. bridgev2 does mark it — `com.beeper.room_type_v2`, `com.beeper.room_features` — but nothing in this repository reads either key yet. This closes the communities half only: a forum's parent space is a portal too (`channel:<id>:-1`) and is not governed by this key.

## The two HTTP surfaces a bridge exposes

All of them live on the appservice listener (WhatsApp 29318, Signal 29328, Google Messages 29336, Telegram 29317 — each bridge's own upstream default), which the reference stack publishes on localhost only.

**Liveness**, unauthenticated: `GET /_matrix/mau/live` and `GET /_matrix/mau/ready`. The compose healthcheck probes `live`, deliberately: `ready` additionally waits for the *network* connection, which without a login never comes, so a bridge nobody has logged in to yet would never be healthy.

**Provisioning**, `GET|POST /_matrix/provision/v3/*` — `login/flows`, `login/start/{flow}`, `login/step/...`, `login/cancel`, `logout`, `whoami`. Authenticated by `Authorization: Bearer <provisioning.shared_secret>`. Three things are worth knowing before writing a client for it:

- The secret must be **at least 16 characters**, or the whole API answers `M_FORBIDDEN` without saying why. `generate-registration.sh` refuses a shorter one, where the message can be read.
- Shared-secret auth still needs **`?user_id=`**: the secret says who may call, the query parameter says whom the call is about, and mautrix checks that user against `bridge.permissions` — which this deployment sets to the single owner. Without it the answer is `M_FORBIDDEN, User does not have login permissions`.
- `GET /_matrix/provision/v3/whoami` is the cheap state probe: it names the network, the login flows, the bridge bot, and the logins that exist (`[]` until a human has paired a phone).

Status goes the other way. `homeserver.status_endpoint` points at the Companion Gateway's webhook, and the bridge POSTs its connection state there authenticated with its own `as_token` — mautrix's only push channel, and the producer behind `bridge.status.changed` on the bus.

## Logging in is a human act

WhatsApp and Signal are paired by scanning a QR code with the phone that holds the account.

Telegram is a sequence of typed answers: a phone number, then the code Telegram sends, then a password if the account has two-factor authentication on. Its QR flow interjects that same password step, so a two-factor account cannot finish even the QR login without answering a question. No Companion screen renders a typed-answer step today, which is what stands between a deployed Telegram bridge and a usable Telegram network (the Companion half of [#175](https://github.com/linagora/twalk/issues/175)); until it lands, the login runs through the provisioning API or the management room. Two things about that session are worth stating where an operator will read them: it is a **full account session**, with the same authority as the user's own app and not a scoped bot token, and Telegram places accounts signing in through unofficial API clients under observation (<https://core.telegram.org/api/obtaining_api_id>). Secret chats are not bridged, so a Telegram conversation reaching Twalk is a cloud chat Telegram itself can read.

SMS through Google Messages is worse, and the honesty about it is part of the feature. Google switched the QR sign-in off for third-party clients in 2024, so this bridge signs in with **seven Google session cookies** — `SID HSID OSID SSID APISID SAPISID` required, `__Secure-1PSIDTS` optional — taken from a **private** browsing window of the user's own Google account, followed by an emoji match on their phone. A private window is required because a normal one rotates the cookies and signing out of it invalidates the session the bridge holds, and Chrome's Device Bound Session Credentials must be off because they bind the session to that machine's hardware key.

Nothing in this repository asks for those cookies, reads them, stores them or automates their extraction. The Companion's screen 3c takes them from the user, the Companion Gateway relays them to the bridge and keeps none ([#57](https://github.com/linagora/twalk/issues/57), ADR 0011). Driving a headless browser to harvest them was considered and **rejected**: it is fragile, and it would make an automated login look like a human session to Google. That decision stands.

Mautrix documents no lifetime for those cookies, so nothing here promises a re-login cadence. When the session dies, the login is repaired from the Companion with the same flow against the same login id.

And the framing this path ships with does not get dropped quietly: SMS transits Google Messages Web, it needs a Google account, it is unavailable to an iOS-only user, and v0.2 replaces it with the sovereign first-party Twake SMS Companion. `sms` stays the network throughout that migration; only the `bridge_id` changes.

No automation replaces any of this, which is why `sensor/tests/bridges_deployment.rs` proves the deployment — Synapse accepts each appservice, each bridge process is live, each provisioning API answers `whoami` with `logins: []` — and never attempts a login.
