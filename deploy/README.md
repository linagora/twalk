# deploy

Deployment manifests: Docker Compose (the v0.1 reference path), Kubernetes overlays (v0.2), and bare-metal Ansible.

`docker-compose/` is the reference deployment: Synapse (the Matrix hub), NATS JetStream (the bus), the Sensor, the Companion Gateway, Hermes with the personas it hosts, and — behind a compose profile — the `mautrix-whatsapp`, `mautrix-signal`, `mautrix-gmessages` and `mautrix-telegram` bridges. Everything is configured through one environment file; copy `docker-compose/.env.example` to `.env` and edit it first. That file documents every variable and is the place to read before this one.

## The stack without bridges

```bash
cd docker-compose
cp .env.example .env    # then edit it
docker compose up -d --wait
```

One step, no manual provisioning: the stack's `provision` one-shot creates the Sensor account on the way up. `./provision.sh` remains for ad-hoc accounts.

## What the Gateway's state directory holds, and what losing it costs

The Companion Gateway keeps its SQLite stores on the `gateway-data` volume (`GATEWAY_STATE_DIR`, `/data` in the container): the owner's device sessions, the append-only consent journal and its outbox, the settings, and the relay's own note that it created the owner's account. Recreating that directory — a fresh volume, a restore from a backup taken before onboarding, a move between hosts — loses exactly those, and nothing on the homeserver.

Two consequences are worth knowing before it happens rather than after:

- **Sign-in still works.** Whether this deployment has its account is asked of the homeserver on every `GET /api/deployment`, whoever created the account ([#133](https://github.com/linagora/twalk/issues/133)); a store with no memory of creating it is not a deployment with no account, and the sign-in screen is offered. Every device is signed out, since the sessions were in the store, and signs back in from the Companion.
- **Consent starts over.** The journal is the single record of who the user decided may be read; a lost journal means every contact is `pending` again until the user decides again — nothing published before is unpublished by it, and nothing decided before is remembered. Back the volume up if that history matters to you.

The registration relay's own refusal does not depend on that row either: the homeserver's `M_USER_IN_USE` closes the window on a second attempt whatever the store remembers.

## Hermes, and the two things it asks of you

Hermes is part of that one step, so `docker compose up -d --wait` gives you a deployment where the whole loop can close: a contact writes, a persona drafts a reply, you approve it in the Companion, and the reply reaches the room. `hermes/tests/full_loop.rs` runs exactly this stack and asserts exactly that.

Two things about it are not free, and both are decisions you are making rather than details:

**It holds the host's Docker socket.** A persona ships as a container image and the runtime starts that image, so the `hermes` service talks to the daemon that runs this stack. A service holding that socket is root on this host: it can start a privileged container, mount your root filesystem and read every other container's secrets. [ADR 0023](../docs/architecture/adr/0023-the-deployment-starts-personas-through-the-hosts-docker-socket.md) argues why this was chosen over running the runtime on the host, over making each persona a compose service, and over a narrower grant that cannot be built from the parts that exist; [`docs/architecture/security-model.md`](../docs/architecture/security-model.md) records it as a residual risk. The personas are handed no socket of their own, and pointing `HERMES_DOCKER_SOCKET` at a **rootless** daemon narrows the grant from root to that account.

If you do not want that trade on this machine, leave `HERMES_PERSONAS` empty: the service starts, hosts nothing, says so in its log, and nothing else in the stack changes.

**It needs a model, and the model has to be reachable from this host.** Twalk ships no LLM and there is no default ([ADR 0015](../docs/architecture/adr/0015-no-default-llm-configured-through-the-companion.md)): set `HERMES_LLM_BASE_URL` and `HERMES_LLM_MODEL`, or Hermes hosts nothing and tells you which variable is missing. The runtime and its persona containers run in the **host's network namespace**, so a proxy on loopback works — which is the reference shape:

```
HERMES_LLM_BASE_URL=http://127.0.0.1:4000/v1    # e.g. a LiteLLM proxy on this host
HERMES_LLM_MODEL=qwen                           # a model that proxy serves, by its name there
HERMES_LLM_API_KEY_FILE=/etc/twalk/llm.key      # a path on this host; it wins over the variable
HERMES_USER_LANGUAGE=fr                         # the language an ambiguous message falls back to
```

That last line is the user's preference rather than yours, and it does one thing: a persona writes its suggestion in the language of the message it is answering, and falls back to this only when it cannot tell — a single word, a greeting, an emoji, a link ([ADR 0016](../docs/architecture/adr/0016-a-suggested-reply-follows-the-conversations-language.md), [#164](https://github.com/linagora/twalk/issues/164)). Leave it empty and nothing breaks: every readable message is still answered in its own language, and each persona says in its log what will happen to the rest.

There is no `HERMES_LLM_PARAMS` line here any more, and that is the point of [#162](https://github.com/linagora/twalk/issues/162). A model that reasons before it answers charges its thinking to the completion budget, and the `assistant` persona used to ask for 300 tokens: the model spent all of them thinking and answered `HTTP 200` with `finish_reason: "length"` and no content, which looked exactly like a model that had said nothing — and was retried. The persona now asks for 2000, that answer is now its own named outcome, and it is refused rather than retried, with a log line saying the budget went to reasoning and where a larger one is set. If your model needs more room than 2000 tokens, `HERMES_LLM_PARAMS={"max_tokens":4000}` is where you say so; it is merged last and wins.

The cost of that namespace is that Hermes and every persona can reach anything bound to this host's loopback. If your endpoint is reachable at a routable address instead, ADR 0023's last section says what to change to put Hermes back on the stack's own network.

Which persona runs is still the user's decision and not this file's: a persona nobody activated is paused, receives nothing, and keeps running (`ADR 0013`). Activation happens on the Companion's `/personas` screen.

## The stack with bridges: two steps, and why

```bash
./provision-bridges.sh                         # 1. registrations, then Synapse
docker compose --profile bridges up -d --wait  # 2. the stack, bridges included
```

**The second step alone does not work, and this is not a convenience we removed — it is a property of Matrix.** A bridge joins a homeserver as an *appservice*, and an appservice is installed by writing its registration file, naming that file in the homeserver's `app_service_config_files`, and restarting the homeserver. Synapse has no API for registering an appservice at runtime; neither does any other homeserver. So nothing that is already running can install a bridge — not the bridge, not the Companion Gateway, not a compose dependency. The bridge facade spec ([#47](https://github.com/linagora/twalk/issues/47)) draws the line in the same place: the Gateway drives logins and reads status, and owns no registration.

What each step does:

1. `./provision-bridges.sh` runs each bridge's own `mautrix-<id> -g` to generate its registration into a volume Synapse reads, re-renders Synapse's configuration with those paths installed, and restarts Synapse if it is running. Generating a registration talks to no homeserver, so this works on a stack that has never started, and it is idempotent: with an unchanged `.env` the registration is byte-identical and Synapse has nothing to notice. Take `./provision-bridges.sh whatsapp` — or `./provision-bridges.sh gmessages` — to provision one bridge only. The names it takes are `whatsapp`, `signal`, `gmessages` and `telegram`: mautrix's own names for the bridges, which is why `./provision-bridges.sh sms` is refused and says so — `sms` is a network and this argument is a bridge (ADR 0005). `telegram` is the one that needs something before it can be provisioned at all: `TELEGRAM_API_ID` and `TELEGRAM_API_HASH`, an application you register at <https://my.telegram.org/apps> against your own phone number. It is the only credential in this deployment that belongs to the network being bridged rather than to your homeserver, no value can be shipped for you, and the script stops with that URL in the message if it is missing.
2. `docker compose --profile bridges up -d --wait` brings the bridges up. Without `--profile bridges` they are ignored entirely — an operator who does not want them pulls no bridge image, runs no bridge container and sets no bridge variable.

One line in `.env` ties the two together and `provision-bridges.sh` checks it before it generates anything: `MATRIX_APPSERVICE_REGISTRATIONS` must name exactly the bridges you provision. Synapse refuses to start on a registration path that does not exist, so a list that is ahead of reality is a homeserver that will not boot.

If you skip step 1 the failure is loud but its shape depends on what `.env` says, so both are worth recognising. With `MATRIX_APPSERVICE_REGISTRATIONS` still empty, Synapse comes up happily and each bridge crash-loops instead, with an appservice authentication error in its log: it asserts its own registration against the homeserver at startup, and Synapse has never heard of it. With the line already set, Synapse itself may refuse to start, because the registration path it is told to read does not exist yet — `up` does start the registration one-shots, but nothing orders them before Synapse, and that ordering is not something a compose dependency can express without dragging the whole profile back into a bridgeless stack. Either way the fix is the same: run step 1.

### What is still a human's job

Connecting a network is a human act and stays one. WhatsApp and Signal are both paired by scanning a QR code with the phone that holds the account: no script can scan it, and the deployment test does not try. Once the bridges are up, the login runs through each bridge's provisioning API — which is what the Companion's networks screens ([#68](https://github.com/linagora/twalk/issues/68)) and the Companion Gateway's facade ([#55](https://github.com/linagora/twalk/issues/55)) exist to put in front of a human — or, until those land, through the bridge's Matrix management room (`!wa login`, `!signal login`, `!gm login`) from your own Matrix client.

SMS through Google Messages needs more than a phone, and the deployment stops well short of it. Google switched off the QR sign-in for third-party clients in 2024, so this login is **seven Google session cookies** — `SID HSID OSID SSID APISID SAPISID` required, `__Secure-1PSIDTS` optional — copied out of a **private** browsing window of your own Google account, followed by an emoji match on the phone. Nothing in this repository asks you for them, reads them or stores them: the Companion's screen 3c takes them, the Companion Gateway relays them to the bridge and keeps none ([#57](https://github.com/linagora/twalk/issues/57), ADR 0011). Harvesting them with a headless browser was rejected — fragile, and it would make an automated login look like a human session to Google — and that decision stands.

What this deployment gives you is a bridge that is up, healthy and waiting. Three things about it are not the deployment's to fix and are yours to know: SMS transits Google Messages Web so this path is not sovereign, it needs a Google account and is unavailable to an iOS-only user, and mautrix documents no lifetime for those cookies, so nothing here promises a re-login cadence. v0.2 replaces this path with the first-party Twake SMS Companion; the network stays `sms` across that migration and only the `bridge_id` changes.

### Choosing which conversations are observed

A bridge builds a portal room **when a conversation becomes active**, not once at login. On the reference deployment one WhatsApp account produced eighteen of them over a single day, as people wrote — and mautrix invites only *the user* into each. So a connected network does not put anything on the bus by itself, and the set of conversations keeps growing for as long as the deployment runs.

`SENSOR_ALLOWED_INVITERS` is necessary and is not sufficient. It has to name each bridge's bot (`@whatsappbot:<domain>`, `@signalbot:<domain>`, `@gmessagesbot:<domain>`, `@telegrambot:<domain>` — each bridge's provisioning API reports its own as `bridge_bot`, which is the value to trust) — that is what lets the Sensor accept an invitation — but it settles only what the Sensor *accepts*, and nothing in mautrix ever asks. What completes the path is the **portal register** ([#105](https://github.com/linagora/twalk/issues/105)): the Companion Gateway reads each bridge's portal rooms as that bridge's own bot, using `GATEWAY_BRIDGE_<ID>_AS_TOKEN` **and** `GATEWAY_BRIDGE_<ID>_BOT_USER_ID`, and invites the Sensor into the conversations the user chooses. The Companion's screen for choosing them is *Conversations watched*, on each connected network's card ([#143](https://github.com/linagora/twalk/issues/143)).

Both variables, and not just the token, because the token alone acts as the **wrong account**. An appservice token used with no `?user_id=` acts as the registration's `sender_localpart`, and `./provision-bridges.sh` generates that localpart — so it is a random string joined to no rooms and never joined to any. The first deployment of the register read 32 portal rooms as `@jaay9HZczkCQNHiIzolLOrvm99Cby90B` and reported `observing=0 invited=0 absent=0 unreadable_bridges=0` ([#171](https://github.com/linagora/twalk/issues/171)). `compose.yaml` names `@whatsappbot`, `@signalbot`, `@gmessagesbot` and `@telegrambot` on your `MATRIX_DOMAIN` by default — the same bots `SENSOR_ALLOWED_INVITERS` has to name, and what each bridge calls its own `bridge.bot_username`; set `WHATSAPP_BOT_USER_ID`, `SIGNAL_BOT_USER_ID`, `GMESSAGES_BOT_USER_ID` or `TELEGRAM_BOT_USER_ID` only if you renamed one.

Telegram is the one where that register does not start empty. It syncs the chat list at login, so roughly fifteen portal rooms appear at once rather than accumulating as conversations become active — a list of decisions waiting, since nothing is observed until the user chooses it. `../bridges/README.md` states the three configuration keys that decide how big that jump is, and what happens if a Telegram group is promoted to a supergroup.

Four things follow, and each is worth knowing before you go looking for a missing message.

- **Nothing is observed by default, and that is deliberate.** Those eighteen rooms held roughly 1,300 memberships, several hundred people who do not know Twalk exists. Observation is chosen per conversation, and until it is chosen the Sensor is in none of them.
- **The deployment can say how many conversations it is outside.** `GET /api/portals` lists every conversation with its name, its size and whether the Sensor is inside; `/metrics` carries the same counts as `twalk_companion_gateway_portal_rooms{observation="observing|invited|absent"}`. The Sensor's own `twalk_sensor_observed_rooms` says how many rooms it is actually reading.
- **A conversation stuck at `invited` is a configuration error with a name.** The Gateway invited the Sensor and the Sensor refused the inviter: that bridge's bot is missing from `SENSOR_ALLOWED_INVITERS`. The Sensor counts those refusals as `twalk_sensor_invites_total{outcome="ignored"}` and logs one warning each.

- **A readable bridge with no conversations says which account it asked as.** Each entry of `bridges` in `GET /api/portals` carries `asked_as` and `joined_rooms`, because `absent: 0` used to mean two different things and a deployment could not tell them apart: a network that has genuinely built no conversation yet, and a register asking an account that is in no rooms. `joined_rooms: 0` next to a `sender_localpart`-shaped account is the second ([#171](https://github.com/linagora/twalk/issues/171)); the Gateway also logs one warning per read, naming the account and the variable.

### The bridge bots are also the accounts that are not people

Those same bot ids belong in `SENSOR_BRIDGE_BOTS` too, and for the opposite reason ([#152](https://github.com/linagora/twalk/issues/152), ADR 0026). `SENSOR_ALLOWED_INVITERS` says whose invitation the Sensor accepts; `SENSOR_BRIDGE_BOTS` says which accounts are the appservices' own service identities, about which nothing is published at all — not presence, not what they write into a portal room, and no consent decision. Leave it empty and the bots are published as contacts: on the reference deployment `@whatsappbot` and `@signalbot` produced 1,150 of 1,216 presence events, two a minute each for as long as the stack ran, and each one travelled through the consent machinery, so the consent state can acquire a row about a robot.

Two lists rather than one, because the second question is not the first and `SENSOR_ALLOWED_INVITERS` also names *you*: deriving the bots from it would delete your own presence — or that of a second account you trust to invite the Sensor — from the bus as a side effect of an unrelated setting. An account you do not name stays a contact, which is the safe failure in this one direction: mistaking a contact for a bot makes a real person disappear from the stream in silence. `twalk_sensor_events_dropped_total{reason="bridge_bot"}` is how you check it worked, and a flat zero on a stack with bridges connected means an id is misspelled rather than that there was nothing to drop.

A bridge whose `GATEWAY_BRIDGE_<ID>_AS_TOKEN` is unset has none of its conversations read at all, and says so in `GET /api/portals` rather than quietly contributing nothing to the totals. `GATEWAY_PORTAL_REFRESH_SECONDS` decides only how fresh the `/metrics` counts are (300 by default, `0` turns the background read off); the API always reads the homeserver there and then. `GATEWAY_CROWD_THRESHOLD` (20 by default) is where a member count becomes a crowd: the chooser asks the user to acknowledge the size of any conversation at or above it before the Sensor is put in, and a conversation whose room is replaced is followed automatically only below it (ADR 0029). The Companion reads the served value and holds no number of its own. `GATEWAY_CONNECTIONS` names the deployment's **connections** (ADR 0033) — the accounts it observes or acts through, the perimeters consent is scoped to; left empty, every bridge is one connection named after its network, plus `matrix` for the user's own account on the homeserver, and the Sensor stamps each event with the connection whose bridge bot built its room, read from the Gateway rather than derived. Declare it to name a second account of one network (each connection named after its `GATEWAY_BRIDGES` entry) or a collector's mailbox or calendar.

## What is where

| Path | What it is |
| --- | --- |
| `docker-compose/compose.yaml` | The stack. Bridges behind `profiles: [bridges]` |
| `docker-compose/.env.example` | Every variable, documented; the file to read first |
| `docker-compose/provision.sh` | Matrix account provisioning (the Sensor, ad-hoc accounts) |
| `docker-compose/provision-bridges.sh` | Step 1 above: registrations, Synapse's configuration, the restart |
| `docker-compose/provision-owner-device.sh` | The operator route of ADR 0034: a **Matrix** device named `Twalk` on the owner's own account, for the Sensor to post approved replies through, its credential written where the Sensor reads it |
| `docker-compose/provision-clerk-device.sh` | The operator route of ADR 0036 (#284): a **Companion Gateway** device named `Buzz` on the owner's session, for the clerk to carry a ✅ made on Buzz through `POST /api/approvals` — listed on the dashboard and revoked there like a phone. The owner's Matrix password at the terminal, one OpenID token, one sign-in, and the refresh token written into `CLERK_GATEWAY_SESSION_DIR` at mode 0600; the Matrix login it makes for the token is logged out at the end. With `--from-owner-device` it makes no login at all and mints the OpenID token with `SENSOR_OWNER_DEVICE_ACCESS_TOKEN` — the device the row above created — which it never logs out: for a homeserver without password login (SSO only) or an operator who is not at a terminal, and it makes the clerk's Gateway device depend on nothing the deployment did not already hold. The password stays the default because a credential you type is one you did not have to store. Idempotent: an alive session is refreshed and kept, a dead one replaced with the other `Buzz` devices revoked |
| `docker-compose/provision-connection.sh` | The operator route of ADR 0033 (#274): the collector's **OIDC grant** for the owner's own account at their organisation's SSO. The collector's own `authorize` command in its own container: a link printed, the sign-in in a browser as `COLLECTOR_OWNER_EMAIL`, the address the browser lands on pasted back (nothing listens at the redirect URI, on purpose), the code exchanged with PKCE, the refresh token written into the `collector-data` volume at mode 0600 and rotated by the collector from then on, and both services asked who the token belongs to — a grant for another account is refused there and then. The client's secret is a **file** on the host (`COLLECTOR_OIDC_CLIENT_SECRET_FILE`), never a variable. Idempotent: a grant in place is left alone; `--renew` replaces one the SSO revoked, which is what the collector's `reconnect_required` on the bus asks for |
| `docker-compose/provision-nostr-key.sh` | A Nostr key for a Buzz writer, generated once into the env file it is given (`BUZZ_PRIVATE_KEY`, with the public half beside it), never into this stack's `.env`. Two writers use it: Hermes — the agent runtime of ADR 0032, not this stack's persona runtime — into its own env file, and the clerk (#265) into a file of its own. Prints the public half, which the relay must be told to accept. `provision-hermes-nostr-key.sh` is the old name, kept as a symlink |
| `docker-compose/provision-buzz-channels.sh` | The operator route of #218: the owner's four Buzz channels, created **as the owner** from a key file only they write, Hermes added as a bot member when its env file holds a key, every `--bot <pubkey>` (the clerk's) added the same way, the UUIDs written where Hermes reads them and the three `CLERK_CHANNEL_*` lines printed for this stack's `.env` — no UUID typed by hand |
| `docker-compose/synapse/homeserver.yaml` | Synapse's configuration template (Jinja2, rendered by the image) |
| `docker-compose/*.Dockerfile` | One image per Twalk component, the clerk's (`clerk.Dockerfile`) and the collector's (`collector.Dockerfile`) included |
| `docker-compose/hermes-entrypoint.sh` | Hermes's entrypoint: start the runtime, or say why it is hosting nothing |
| `docker-compose/clerk-entrypoint.sh` | The clerk's entrypoint: start the clerk, or say why it is hosting nothing; hand its key and, since #284, its Gateway session directory to the unprivileged account it runs as, and refuse a write half that is half set, naming the script |
| `docker-compose/collector-entrypoint.sh` | The collector's entrypoint: hand the mounted client secret to the unprivileged account it runs as, refusing a secret that is a directory (a path that did not exist when the container started), empty, or readable by others, naming the variable |
| `docker-compose/run-persona-image.sh` | The argv every persona in `HERMES_PERSONAS` names, shipped in the Hermes image as `run-persona` |
| `../bridges/` | Each bridge's base configuration, and the generator the one-shots run |

Covered end to end by `sensor/tests/deployment.rs` (the pipeline, bridges off), `sensor/tests/bridges_deployment.rs` (the two-step procedure above, bridges on), `companion-gateway/tests/deployment.rs` (the Companion's origin) and `hermes/tests/full_loop.rs` (the whole loop, on this stack's own Hermes).
