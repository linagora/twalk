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

That last line is the user's preference rather than yours, and it does one thing: a persona writes its suggestion in the language of the message it is answering, and falls back to this only when it cannot tell — a single word, a greeting, an emoji, a link ([ADR 0016](../docs/architecture/adr/0016-a-reply-follows-the-conversation-not-the-user.md), [#164](https://github.com/linagora/twalk/issues/164)). Leave it empty and nothing breaks: every readable message is still answered in its own language, and each persona says in its log what will happen to the rest.

There is no `HERMES_LLM_PARAMS` line here any more, and that is the point of [#162](https://github.com/linagora/twalk/issues/162). A model that reasons before it answers charges its thinking to the completion budget, and the `assistant` persona used to ask for 300 tokens: the model spent all of them thinking and answered `HTTP 200` with `finish_reason: "length"` and no content, which looked exactly like a model that had said nothing — and was retried. The persona now asks for 2000, that answer is now its own named outcome, and it is refused rather than retried, with a log line saying the budget went to reasoning and where a larger one is set. If your model needs more room than 2000 tokens, `HERMES_LLM_PARAMS={"max_tokens":4000}` is where you say so; it is merged last and wins.

The cost of that namespace is that Hermes and every persona can reach anything bound to this host's loopback. If your endpoint is reachable at a routable address instead, ADR 0023's last section says what to change to put Hermes back on the stack's own network.

Which persona runs is still the user's decision and not this file's: a persona nobody activated is paused, receives nothing, and keeps running (`ADR 0013`). Activation happens on the Companion's `/personas` screen.

### How far back a grant reaches

Granting a contact answers the messages they have just sent. The window is `HERMES_GRANT_REACH_SECONDS`, **an hour** unless you say otherwise, and it exists because of the only order these two things ever happen in: a message lands, you read it, and *that* is when you decide about the person who sent it. Before [#364](https://github.com/linagora/twalk/issues/364) the message you had just watched arrive was the one message that would never be answered — the label is stamped at arrival and nothing went back for it — and the consent screen told you, in five languages, to grant somebody and then wait for their next message. Now every message from that contact that arrived within the window, on a connection the decision covers, and was still waiting for a decision, wakes the persona through the ordinary path: same trigger id, same arrival time, an ordinary suggestion, and nothing republished ([ADR 0040](../docs/architecture/adr/0040-a-grant-reaches-the-messages-still-waiting-for-it.md)).

What you set it to is a judgement about how long a message stays worth answering, and the default is the hour a draft stays approvable for — the same judgement, deliberately the same number. `0` turns it off and a grant means nothing in the past tense again. Above `86400` the personas **refuse to start**, naming the variable: past the bus's own duplicate window ([ADR 0037](../docs/architecture/adr/0037-the-bus-keeps-ninety-days-and-two-gigabytes-and-no-more.md)) the same message can be answered twice, and inside it the suggestion's deterministic id makes a second replay collapse on the bus, which is what makes "revoke, grant again" safe rather than merely unlikely.

`GATEWAY_GRANT_REACH_SECONDS` defaults to whatever you set above and should not be set to anything else. The Gateway does not act on the reach — the personas do — it **states** it: `/consent` says how many of a contact's messages a grant will answer *before* you grant, counted from the bus per request and stored nowhere, and names the window in its own words. Two different numbers there and the screen promises what the personas will not do.

Two consequences worth knowing before you meet them. A grant taken while no persona has ever run is missed, because each persona's decision consumer starts at the bus's head rather than replaying every decision you ever took; and a persona that was down longer than the reach comes back and does **not** answer what was granted while it was away, which is the same rule applied to itself. In both cases nothing is answered and nothing is logged as an error, because neither is one: the remedy is the one you already had, which is to write to the person yourself.

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

### A bridge that blinks

`mautrix-whatsapp` loses its websocket and reconnects inside the second, about five times in twelve hours on the reference deployment (`Error reading from websocket: … EOF`, once `Got 503 stream error, assuming automatic reconnect will handle it`). Nothing is lost and there is nothing to fix on the bridge — whatsmeow does it by itself.

What was wrong was treating it as an event. Each blip wrote two rows into the Gateway's store and two lines into the owner's activity channel, pairs that cancel themselves, in the one feed that exists to say *something needs you* ([#324](https://github.com/linagora/twalk/issues/324)).

So the Gateway **holds** a state worse than `connected` for `GATEWAY_BRIDGE_STATUS_GRACE_SECONDS` (30 by default, `0` for the old behaviour) and publishes nothing if the bridge is back before it elapses: no row, no event, no line, because nothing happened the owner can act on. A degradation that outlives the grace is published with the instant the bridge **first** reported it, so your clock does not shift because the Gateway waited. The announcement itself can be up to a quarter of the grace later than that — the loop that publishes what was held ticks at `grace / 4` — so 30 seconds means an outage is said within about 37. `connected` is never held — a recovery is good news and goes out at once.

What to watch on `/metrics`: `twalk_companion_gateway_bridge_statuses_total{channel,state}` gained two words that are not states a bridge reports — `held`, one waiting out its grace, and `settled`, one that came back inside it. `held` climbing with no `settled` following is a bridge that really went down; the two climbing together, five times in a night, is the blinking this exists for. In the log: *"a bridge reported a state worse than connected; held in case it comes back"*, then either *"a bridge blinked and is back inside the grace"* or *"a bridge changed state and stayed changed"*.

What it does **not** cover: the collector's own connection statuses (`connection.status.changed.v1`, #275). An SSO or a JMAP server that blinks would flap the same way, and the mechanism is not shared — it lives on the Gateway's outbox, which the collector does not have, and nobody has measured the collector's connections flapping. If they are ever seen doing it, this is the shape to copy.

### What is still a human's job

Connecting a network is a human act and stays one. WhatsApp and Signal are both paired by scanning a QR code with the phone that holds the account: no script can scan it, and the deployment test does not try. Once the bridges are up, the login runs through each bridge's provisioning API — which is what the Companion's networks screens ([#68](https://github.com/linagora/twalk/issues/68)) and the Companion Gateway's facade ([#55](https://github.com/linagora/twalk/issues/55)) exist to put in front of a human — or, until those land, through the bridge's Matrix management room (`!wa login`, `!signal login`, `!gm login`) from your own Matrix client.

SMS through Google Messages needs more than a phone, and the deployment stops well short of it. Google switched off the QR sign-in for third-party clients in 2024, so this login is **seven Google session cookies** — `SID HSID OSID SSID APISID SAPISID` required, `__Secure-1PSIDTS` optional — copied out of a **private** browsing window of your own Google account, followed by an emoji match on the phone. Nothing in this repository asks you for them, reads them or stores them: the Companion's screen 3c takes them, the Companion Gateway relays them to the bridge and keeps none ([#57](https://github.com/linagora/twalk/issues/57), ADR 0011). Harvesting them with a headless browser was rejected — fragile, and it would make an automated login look like a human session to Google — and that decision stands.

What this deployment gives you is a bridge that is up, healthy and waiting. Three things about it are not the deployment's to fix and are yours to know: SMS transits Google Messages Web so this path is not sovereign, it needs a Google account and is unavailable to an iOS-only user, and mautrix documents no lifetime for those cookies, so nothing here promises a re-login cadence. v0.2 replaces this path with the first-party Twake SMS Companion; the network stays `sms` across that migration and only the `bridge_id` changes.

### When the acting device is revoked

The Sensor posts an approved reply as a **device of the owner's own account**, because a mautrix bridge relays to its network only what the logged-in user's own account sends ([ADR 0025](../docs/architecture/adr/0025-twalk-acts-as-the-user-through-a-device-of-their-account.md)). That is a long-lived access token for the owner's account, sitting in `SENSOR_OWNER_DEVICE_ACCESS_TOKEN`, and the mitigation the ADR named for it was neither encryption nor scope: it is **a device among the owner's devices**, which they can revoke from any Matrix client without asking anybody here.

A mitigation nobody can observe is not one, so [#229](https://github.com/linagora/twalk/issues/229) made the revocation visible. The owner deletes the device from their phone; the next thing Twalk asks the homeserver under that token is refused with `M_UNKNOWN_TOKEN` — its sync, or the send of a reply approved in the half-minute before the sync's long poll comes back; and then, without a restart:

- one `ERROR` naming the credential, what puts it back, and — deliberately — that this is *not* a homeserver that is merely unreachable, because those are two situations with two remedies and they used to share one silence;
- `twalk_sensor_owner_device_credential_gone 1` on `/metrics`, for a deployment whose logs nobody is reading;
- the sync loop **ends**, rather than retrying a token nothing in this process can renew;
- and every approved reply for a **bridged** conversation is refused rather than posted. Not posted as `@sensor:` either: that returns an event id from Synapse and reaches nobody, which is the outcome this whole arc exists to remove. The refusal is *transient*, so the approval is never acknowledged on the bus: re-provision and restart the Sensor inside the retry schedule and the bus hands that reply to the new process, which sends it as the owner. Let the schedule run out and it is dead-lettered with that same sentence as its reason, which is what the approval screen shows the owner ([#311](https://github.com/linagora/twalk/issues/311)).

All of that happens after the owner has pressed the button, and [#404](https://github.com/linagora/twalk/issues/404) closed the half of #229 its own words asked for and its criteria did not: *"the Companion stops offering a delivery it can no longer perform."* The Sensor now says the device's state **on the bus**, as `owner.device.state.changed.v1` ([ADR 0041](../docs/architecture/adr/0041-the-owner-device-says-on-the-bus-whether-it-can-act.md)) — at the start of every run and at each transition, including a *send* refused with `M_UNKNOWN_TOKEN`, so the gauge and the bus never disagree — and the Companion Gateway follows that one subject from its last event, holds the state in memory, and answers `cannot_reach` with `owner_device_credential_gone` for a bridged conversation while the credential is gone. The approval screen then says, before anything is attempted, that no reply can be posted as the owner and that onboarding again hands over another device. Nothing is stored for this: a Gateway that has heard nothing answers exactly as it did before #404, because an absence must not read as a revocation, and the Sensor's republication at every start is what makes that gap unreachable in practice. There is no new variable and nothing to expose — it travels on the bus both components already hold.

A **native** Matrix conversation is unaffected: no bridge stands between the room and the person reading it, so the Sensor's own account posting there always reached the contact and still does.

The gauge is the one to alert on, because from the moment it is `1` no reply to a bridged conversation can leave and every approval the owner makes ends in a dead letter:

```promql
twalk_sensor_owner_device_credential_gone > 0
```

Two counters beside it belong to the handover itself. `twalk_sensor_handovers_held_total` is onboarding working: each increment is one credential this Sensor persisted, brought up and acknowledged in the handover room. `twalk_sensor_handovers_refused_total{why}` is the channel's security property made observable — a to-device event can be addressed to this Sensor by **any account on any homeserver**, so `not_encrypted` or `unexpected_sender` climbing with no onboarding in progress is somebody trying, and there is nowhere else that would ever show it:

```promql
increase(twalk_sensor_handovers_refused_total[1h]) > 0
```

There are two remedies, and since [#228](https://github.com/linagora/twalk/issues/228) the owner has one of their own: **onboarding again in the Companion** creates a new device on their account and hands its credential to the Sensor Olm-encrypted, which the Sensor writes to `SENSOR_STATE_DIR/owner-device.json` and acts through **without a restart** — `twalk_sensor_handovers_held_total` climbs, the gauge above goes back to `0`, and a reply still inside its retry schedule goes out. The operator's remedy is the one it always was: `docker-compose/provision-owner-device.sh` and a restart of the Sensor. Either one clears the approval screen too, with nothing reloaded there: the Sensor publishes `present` as it brings the new device up, and the next read of the listing offers the reply again. No invitation to re-accept: the new device joins the portals as the bridges' own bots invite it, which is the same path the first one took. The restart is there because the credential is an environment variable an operator sets; [#228](https://github.com/linagora/twalk/issues/228) is the handover that removes that step.

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

## The collector: your own mailbox and calendar, two steps, and why

```bash
./provision-connection.sh                        # 1. your grant: a sign-in at your SSO, once
docker compose --profile collector up -d --wait  # 2. the stack, collector included
```

The collector is the one service of this stack that reads an account of **yours** rather than a network you talk on: your mailbox over JMAP and your calendars over CalDAV, at your organisation's own services, through one OIDC grant for your own SSO account ([ADR 0033](../docs/architecture/adr/0033-twalk-perceives-what-involves-other-people.md), [#251](https://github.com/linagora/twalk/issues/251)). It lives behind the `collector` compose profile — singular, one collector for one grant, as #274 named it and as the entrypoint's own messages say — so a deployment that does not want it pulls no image, runs no container and sets no `COLLECTOR_*` variable. What it publishes goes through the same gate as everything else: a mail from a person is a message on the bus with its sender as a contact, pending until you decide ([#276](https://github.com/linagora/twalk/issues/276)); a change in your calendars is an event whose participants are withheld when you revoked them ([#280](https://github.com/linagora/twalk/issues/280)); a reply you approve leaves from your own mailbox ([#278](https://github.com/linagora/twalk/issues/278), [ADR 0038](../docs/architecture/adr/0038-an-oidc-grant-is-the-users-identity-and-a-jmap-submission-their-device.md)); and Hermes's one pull, your free/busy, is capped, relayed through the Gateway and recorded ([#281](https://github.com/linagora/twalk/issues/281)).

**The first step is a human's, and there is no way around it.** A grant is a refresh token the SSO issues to a signed-in user, and the user is you: `./provision-connection.sh` runs the collector's own `authorize` command in its container, prints a link, you open it in a browser, sign in as `COLLECTOR_OWNER_EMAIL`, and paste back the address the browser lands on — nothing listens at `COLLECTOR_OIDC_REDIRECT_URI` on purpose, since a listener would be a port to protect. The collector exchanges the code with PKCE, asks both services who the token belongs to, refuses a grant for anybody else there and then, and writes yours into the `collector-data` volume at mode 0600, where it renews and rotates it from then on. Before that, three things the script checks are yours to bring: a **confidential** OIDC client at the SSO allowed the code flow with PKCE and the `offline_access` scope, its **secret in a file** of your own at mode 0600 named by `COLLECTOR_OIDC_CLIENT_SECRET_FILE` (never in `.env`, never on a command line — [#239](https://github.com/linagora/twalk/issues/239)'s lesson, and the entrypoint refuses a looser file), and the two connections named in `GATEWAY_CONNECTIONS` (`kind` `email` and `calendar`) and in `COLLECTOR_MAIL_CONNECTION` / `COLLECTOR_CALENDAR_CONNECTION` — with `GATEWAY_SERVICE_TOKEN` set, which is how it reads the registry, the collector refuses to start on a connection the Companion Gateway does not know; without the token it says so at start and runs unchecked, which is the shape the deployment test uses.

**Step 2 alone does start** — that is the difference from the bridges, and it is deliberate. Without a grant the collector comes up (`--wait` returns once the container runs — the service has no healthcheck, so "up" means "still running a few seconds later", which is what the test checks), connects to the bus, and says on it what an operator needs to hear: one `connection.status.changed.v1` per connection, `reconnect_required`, the service `sso`, and in the hint the command to run and who to sign in as. The same words are in `docker compose logs collector` at `WARN`, and the Companion shows them on the connection's card. That is the state `collector/tests/deployment.rs` proves a fresh deployment reaches, since no test can sign in for you. An SSO that does not answer when the stack comes up is not a crash either: the collector reads the SSO's discovery document the first time it has a grant to renew, says `unreachable` on the bus until it answers — the grant left alone, since nothing said it was refused — and retries every round.

What each variable is for is in `.env.example`'s collector section; the seam with the Gateway is two variables that follow from `GATEWAY_SERVICE_TOKEN` by default (`COLLECTOR_GATEWAY_URL`, where the registry is read, and `COLLECTOR_HTTP_LISTEN`, where the Gateway relays a free/busy read), and `GATEWAY_COLLECTOR_URL` is the one you set by hand — `http://collector:8090` with the profile up.

**A calendar service is the ESN, not the DAV server and not the calendar app.** Measured three times over on one account (2026-09-24): the app's host serves a web interface, the DAV host serves `/calendars/…` and answers `/api/user` with a sabredav error, and only the **ESN** serves both `/api/user` and a relayed `/dav/`. Point `COLLECTOR_CALDAV_URL` at the ESN — for a Twake Workplace account, the `twcalendar.` host, not the `dav.` one and not the calendar application's.

**Which services these URLs must name** is worth knowing before the first `authorize` rather than after it — and each is asked for only when its connection is held (#321), so a deployment that reads a mailbox and no calendar sets one URL, sends nothing to a calendar service, and hears nothing about one. `COLLECTOR_JMAP_SESSION_URL` is a JMAP session document (RFC 8620) whose account is yours, and the push the collector prefers is that server's RFC 8887 WebSocket, with the ticket TMail asks for; a server that serves no push is polled every `COLLECTOR_MAIL_POLL_SECONDS` and says so once. `COLLECTOR_CALDAV_URL` is narrower than "a CalDAV server": it is the root of an **OpenPaaS side service** — `GET /api/user` answering a user document with `preferredEmail` and `_id`, the HAL list of your calendars at `/dav/calendars/<id>.json`, then sabre's `PROPFIND`, `REPORT calendar-multiget` and `free-busy-query` underneath. A Twake Workplace deployment serves that at its `sabre-dav` host. A **Cozy** instance does not — its calendars live behind Cozy's own API and its `/api/user` answers `You must be authenticated` in `text/plain` to any Bearer — and neither does a bare CalDAV server with no `/api/user` at all. Both are what [#320](https://github.com/linagora/twalk/issues/320) found on a real account, so the collector now says what it met rather than one sentence for every refusal — at `authorize` and on every health round alike. A service that asks for a bearer and refuses yours is a missing audience or a scope at the SSO. A service that asks for something else, or that offers no challenge at all, is not taking the SSO's tokens there, and no scope will change that answer. A URL that answers something else entirely — a `404` where the user document should be — is reported as unreachable with the status it gave, which is the other way this ends up wrong: the URL, not the client.

**A calendar on another credential is a second collector** (#342). The mailbox's grant is the organisation's SSO speaking for its owner (ADR 0038); a calendar service that challenges `Basic`, or sits behind another SSO, does not read it — which is what the reference deployment's does, measured in #320. So the credential belongs to the **connection**: `COLLECTOR_CREDENTIAL=basic` with a `COLLECTOR_BASIC_USER` and a password file at 0600, and because one process holds one credential, that connection is a process of its own — the `collector-calendar` profile, its own `CALENDAR_COLLECTOR_*` block in `.env`, its own volume. The two collectors share the bus and nothing else: each consumes approvals under a durable consumer named for its own connection, and the calendar's password never sees the mailbox's grant. Point `GATEWAY_COLLECTOR_URL` at whichever of them holds the calendar connection, since that is the one that serves a free/busy read.

### The production proofs

The collector's deployment is proved by four things happening on a real deployment, each with the line of log or the metric that shows it, so that "it works" is a fact somebody can read again later. They are written here as the procedure; the record of the first run is at the end. Read the bus through the Companion (the connections' cards on `/networks`, the drafts on `/approvals`) or through the Companion Gateway's API (`GET /api/connections`, `GET /api/suggestions`); the collector's `/metrics` is on `COLLECTOR_METRICS_LISTEN` inside the compose network.

1. **The grant is the owner's, and the owner is never a contact.** After step 1, `docker compose logs collector` says `the connection is connected` for both connections, and `GET /api/connections` shows them `connected` with no hint; `twalk_collector_connection_state{connection="…",state="connected"}` is `1` for each. The collector's whoami printed by `provision-connection.sh` named `COLLECTOR_OWNER_EMAIL` at both services — the grant is yours and nobody else's. A mail you send yourself is **not** on the bus (`twalk_collector_events_dropped_total{reason="owner"}` moves, no `inbound.message.received.v1` is published): the owner is never a contact ([ADR 0021](../docs/architecture/adr/0021-the-owner-is-never-a-contact-on-any-event.md)).
2. **A mail from a granted contact reaches Hermes, and the approved reply reaches them from your mailbox.** Grant the contact on the mail connection in the Companion (`consent.state.changed.v1` with `granted`, scope the mail connection). Have them write to your INBOX. Within seconds — push is on when TMail's session offers it: `push is on: a delivery wakes the mail poll` in the log, `twalk_collector_push_connected 1`; within `COLLECTOR_MAIL_POLL_SECONDS` otherwise — the mail is `inbound.message.received.v1` on the bus (`published fr.linagora.twalk.inbound.message.received.v1` in the log, `twalk_collector_events_published_total{type="fr.linagora.twalk.inbound.message.received.v1"}` moves), the persona wakes Hermes, Hermes's answer comes back through the Gateway (`hermes answered: the suggestion is on the bus` in the Gateway's log) and is on `/approvals`. Approve it. The collector logs `an approved reply left from the owner's mailbox` and reports `reach=contact`; the reply is in your **Sent** folder, in the thread, from your own address, and in the contact's inbox. **Open that mail and read it.** "It was submitted" and "it says something" are two different facts, and the first production reply was submitted, reported `reach=contact`, and arrived blank: TMail accepted a create whose body part it does not read, stored no body, and sent the empty result without a word of refusal ([#332](https://github.com/linagora/twalk/issues/332)). A proof that stops at the log line would have recorded that as a success. A refusal instead is on the approval screen with its code.

   **What the approval says it answers** (#360): a suggestion produced through Hermes carries `data.context` — a `summary` Hermes wrote, in its own words and never a quotation, and the `contact` the Gateway took from the trigger it validated — the display name the message carried, not the identity, because on a bridged network the identity is a phone number inside a Matrix ID and this line is read by a human. The approval screen and the clerk's Buzz post both show it above the draft, so a decision is taken knowing what it answers. **For an existing Hermes to start sending one, its route's prompt must ask for a `summary` beside the `reply` and the `language` it already asks for.** In `config.yaml` on the Hermes host, under `platforms.webhook.extra.routes.<your route>.prompt`, the last line asks for the answer's shape; add the member to it and say what it is:

```
Answer with ONE JSON object and nothing else, no code fence:
{"reference": "{reference}", "reply": "<the reply text>", "language": "<the BCP-47 tag of the language you wrote the reply in>", "summary": "<in one or two sentences, in the same language, what this message asks — your own words, never a quotation of it, at most 280 characters>"}
```

Until that line changes, answers keep working and the suggestion carries no context, which is the screen as it was before: an agent that predates a member is not a broken one. A summary that arrives empty or over 280 characters is refused with its own code and nothing is published — a blank line where the context should be is the silence the member exists to end.

   **What the agent is given to answer with** (#362): a mail's **subject** crosses the seam beside its body, because it is the sender's own words written in the same breath and it is often where the ask lives — *"RDV ?"* over four lines of pleasantries. A message with no subject (any bridged network, and a revoked sender's mail) carries no member at all rather than an empty one. **This moves the template to version 2, and that is a breaking change in one direction: a route whose `filters` pin `template_version` to `1` stops matching and answers nothing.** Neither order of deployment is safe on its own — pin `2` before the personas send it and the route goes silent; deploy the personas first and it goes silent the other way — so widen the filter, deploy, then narrow it. Three steps in `config.yaml`, under `platforms.webhook.extra.routes.<your route>`, and no window where a message goes unanswered:

```yaml
# 1. on the Hermes host, first: accept both shapes, and add the line below
filters:
  - field: template_version
    in: [1, 2]
```

   2. deploy the personas, which now send version 2;
   3. pin the filter back to `equals: 2`, so that a version 3 is a decision rather than a surprise.

   The prompt's new line goes in at step 1, beside the message:

```
The message: {message}
The message's Subject line: {title}
```

   **A message with no Subject line leaves that line unfilled**, and not blank: Hermes's renderer puts the placeholder back when a member is absent, so the prompt reaches the model with the token still in it. Say so once in the prompt or the model reads the token as the subject — *"a bridged message has no Subject line; when the line above still shows a placeholder instead of text, there is none, and it is not something to quote."* A prompt that never mentions the Subject line at all keeps working on version 2: the member is simply unread, which is the silence this ticket started from.

   **A prompt that forbids acting gets a draft that guessed** (#363). Measured on the reference deployment on 2026-09-24: a draft accepted a meeting slot — *"Le mardi 13 octobre à 14h me convient très bien"* — and the Gateway's record of free/busy reads for that whole day was empty. The cause was not a missing capability. Hermes's webhook adapter hands the rendered prompt to the ordinary agent path (`deliver_only` is what skips the agent, and this route does not set it), so the drafting agent had its tools, the skill was installed, and `TWALK_GATEWAY_URL` and `TWALK_ANSWER_SECRET` were both in its `.env`. What it did not have was an instruction to look and a connection to name: the prompt never mentioned the calendar and closed with *"Answer with ONE JSON object and nothing else"*, which to a model deciding whether to act reads as **do not act, emit**. Two lines fix it, and `TWALK_CALENDAR_CONNECTION`, which the skill's own install step (4, below) adds, is the third:

```
If this message proposes a time, asks when the user is free, or asks to move
a meeting, read the user's free/busy with the twalk-calendar skill BEFORE you
write, and propose or accept only what the calendar leaves free. Take as many
turns as you need to do that; your LAST message is the JSON object below and
nothing else. If you could not read the calendar, say so in the reply and
choose no time — a guessed time is a commitment made in the user's name.
```

   The sentence about turns is the one to keep when trimming. A route's prompt asking for one object and nothing else is read as a ban on tool calls, and an agent that may not act can only invent.

   **And it may stop and ask you** (#367), which is the other half of the same freedom: the draft that matters sometimes depends on one thing only the owner knows — which of two projects a message is about, whether they want to meet this person at all. An agent with nowhere to put that question invents an answer to it. So the answer has a second shape: the reference, and `deferred`, the question it asks, and no reply at all. The Gateway records it in the owner's journal, counts it under `deferred`, answers `200` and publishes nothing — because nothing went wrong and there is nothing to approve yet. Before this it was read as an unreadable answer and counted as a refusal, which was a statement that something had. The question itself reaches the owner through their own channel, which is Hermes's side of the seam: the prompt names that channel, and the runbook cannot, because it is the owner's.

```
If you need something only the user can tell you before you can write a good
reply — which project this is about, whether they want this meeting at all —
ask them in their own channel, and then answer with the reference and a
`deferred` member saying what you asked, and no reply:
{"reference": "{reference}", "deferred": "<what you asked them, in your own words>"}
Their answer will reach you there; write the draft then.
```

   One rule holds whichever shape the answer takes: what you say about the message is **your own words, never a quotation of it** — the rule #360 set for the summary, applied to the question, because a contact's mail quoted into the owner's channel is the contact's words travelling somewhere they were not sent (ADR 0012).

   **The clerk's post says it too** (#377), so Buzz alone is enough to decide from: the reads and the questions appear as lines under the context and above the text to approve. It costs no extra request — the clerk already reads `GET /api/suggestions/{id}` before every post, to learn whether the reply can reach the contact at all, and the path is a member of that same answer. A Gateway too old to send one, or one the clerk cannot read, posts exactly as before.

   **And the approval screen shows what the draft did before it wrote** (#367's other half): the reads it made of your calendar, the questions it put to you, oldest first, folded under one line on the card. That is what makes the autonomy safe to want — an agent that may read and ask is more useful than one that guesses, and less transparent, so the approval has to cover the path as well as the text. It works because of a decision taken for another reason: every governed read records the `delivery` its caller named it by, and the skill tells the agent to put **the reference it was given** there, so a read made for this message carries this message's id and a read made for another does not. An agent that does not pass the reference still reads the calendar; its reads simply do not appear on any card, which is the cost of not naming them.
3. **A change in your calendar is an event, its participants reduced as you decided.** Create a meeting in your calendar with a contact you granted on the mail connection and one you revoked: within `COLLECTOR_CALENDAR_POLL_SECONDS` the bus carries `calendar.event.created.v1` (`published fr.linagora.twalk.calendar.event.created.v1` in the log) with the granted participant named and `participants_withheld` counting the revoked one; move it, and `calendar.event.changed.v1` names the fields that moved; delete it, and `calendar.event.removed.v1` carries the title. No description and no attachment leaves the collector at any point, and no location either until you say so: **where** a meeting is travels only once the owner turns it on in the Companion's settings (#354), off as this ships. Opening it takes effect on the next calendar poll — the collector reads the decision before each one — and the log says which way it went, at `warn` when it is on, because that is the direction in which something leaves the machine.
4. **"Are you free Thursday?" is answered with slots that are really free.** Install the skill on Hermes's side: copy `skills/twalk-calendar/` into its skills directory and add `TWALK_GATEWAY_URL` (the Gateway's origin) and `TWALK_CALENDAR_CONNECTION` (the calendar connection this deployment reads) beside `TWALK_ANSWER_SECRET` in its `.env`. **Then give the agent the tool**, because a skill it cannot run teaches it nothing — in `config.yaml` on the Hermes host:

```yaml
mcp_servers:
  twalk-calendar:
    command: python3
    args: ["/home/<owner>/.hermes-twalk/skills/twalk-calendar/mcp_server.py"]
    # By reference, so the secret stays in .env. Hermes filters what a child
    # process inherits, so a server whose variables are not named here starts
    # fine and refuses every read — measured, 2026-09-24.
    env:
      TWALK_GATEWAY_URL: "${TWALK_GATEWAY_URL}"
      TWALK_ANSWER_SECRET: "${TWALK_ANSWER_SECRET}"
      TWALK_CALENDAR_CONNECTION: "${TWALK_CALENDAR_CONNECTION}"
```

   That server is one stdlib-only file beside the skill; it offers `freebusy` and `event_facts`, signs them with the same secret, and can reach nothing else. The `env:` block is not optional: without it the three variables do not reach the child, the server answers every call with the name of the one that is missing, and the draft says it could not check the calendar — which is the right behaviour and not the one you wanted. MCP servers reach every platform's toolset by default on this Hermes, so the drafting route holds them with no `platform_toolsets` line. **Do not give that route a terminal instead.** Measured on 2026-09-24, with a shell for one afternoon, the drafting agent's first two commands were a `cat` of a `.env` and a read of `SOUL.md` — the gestures of an agent finding its way, and exactly what a paragraph in a stranger's mail could aim at something else ([ADR 0039](../docs/architecture/adr/0039-the-drafting-agent-is-graded-by-what-it-can-reach.md)). Web search and the other skills stay available and are meant to be: what is graded is what a capability can reach, not how much the agent may think. Have a granted contact ask, on any network, when you are free. Hermes reads your free/busy through the Gateway (`hermes read the owner's free/busy` in the Gateway's log, `twalk_companion_gateway_hermes_reads_total{outcome="served"}` moves, a `hermes_read` row in its store; `a free/busy read was served` in the collector's log) and its suggestion proposes times that fall in the gaps of your calendar and none that overlap a busy interval. A read refused — a window wider than fourteen days, a calendar connection not connected — is a row with its code and a suggestion that says the calendar could not be checked, never a guess.

   **A calendar written by Outlook is read** (#350). An iCalendar `TZID` is whatever the client that wrote the event put there, and there are two families in the wild: IANA names (`Europe/Paris`) and Windows names (`Romance Standard Time`), which are labels of Microsoft's own table. Measured on the reference deployment on 2026-09-24, in one window: 57 events with `Romance Standard Time`, two with `W. Europe Standard Time`, one with `GMT Standard Time` — sixty of the owner's real meetings, read, refused and never published, which made their free/busy a description of a calendar they do not have. The collector now reads both families through CLDR's `windowsZones.xml`, generated into the binary with its release recorded (`collector/tools/generate-windows-zones.py`), and publishes the **IANA** zone whatever the calendar wrote. A TZID in neither family is still refused and still named in the log — an instant nobody can place must not be published an hour wrong — and a calendar's refusals are counted twice over, because sixty lines and no number is how this went unnoticed for two days: one line per poll (`resources of this calendar were read and not published refused=… reasons={…}`) and `twalk_collector_calendar_resources_refused_total{reason}` on `/metrics`, whose three reasons are `zone_unknown`, `zone_windows_unmappable` and `unreadable` — a number to alert on rather than a line to grep for.

   **The read carries the owner's own time with it** (#369): the zone their calendar declares (`CALDAV:calendar-timezone`), where that name came from, and what hour it is there now. A free/busy answer is a list of UTC instants and every sentence a contact reads is in local time, so something converts — and before this the thing converting was a model with nothing to convert *to*. The Gateway's log line names the zone on every read, and `timezone_source` says which of two things answered.

   **`calendar`** is the collection declaring it, asked first. **`events`** is the fallback, and on the reference deployment it is the only one that fires: measured on 2026-09-26, its collections declare no `calendar-timezone` at all, which is the ordinary state of a server whose clients never set one. So the collector counts the `TZID` of the owner's own events — which it already reads on every event it publishes — and answers the zone most of their agenda is written in. The mode, not the latest: one invitation authored in Tokyo does not move the owner to Tokyo, and the collector's log says how many events it counted.

   **And the read hands over the free gaps, not the arithmetic** (#379). Beside the busy intervals it answers `free`: the stretches nothing occupies, each with its UTC pair, the same pair in the owner's zone, and its length in minutes. That member exists because of one measured failure: on 2026-09-26 a draft read the calendar three times, was told `Europe/Paris`, and proposed two hours that overlapped a meeting — it had found the gaps in the UTC intervals and written them as if they were local hours. The skill now tells the agent to name a time **only** from `start_local`, and never to convert an instant itself. An agent that copies cannot make that mistake; one that converts already has.

   **And a day may run other hours than the rest** (#386). The amplitude above is the default; the settings screen also holds the days that differ — a short Wednesday, a Friday that ends at 16:00 — keyed by weekday, and the gaps of each day are clipped by that day's own hours. One amplitude for a whole week is often false, and before this the owner's only way to say so was to make the whole week as narrow as its narrowest day, which hides four afternoons from every draft. A day with no exception runs the default: absence means *as usual* and never *no meetings*, since the days they accept are already a list of their own. A deployment with one amplitude answers the document it answered before — `exceptions` is absent, not empty — so an older collector reads it unchanged.

   Nothing is dropped for being short or nocturnal **unless the owner has said what their hours are** (#381). The Companion's settings screen holds one amplitude and a set of days — a start, an end, and which weekdays they accept meetings on — journalled like every other decision here, and the collector reads it before it answers. The gaps are then clipped to it, at the edges too (07:00–09:30 against a day starting at 09:00 is a half-hour offer, and half an hour is a meeting), the busy intervals are untouched, and the answer carries the amplitude so an agent can say *"I am taken on my usual hours that week"* rather than *"I have no slot at all"*. Shipped unset, which means the deployment has no opinion and every gap is offered — there is no default working day, because a default would be a decision about somebody's life taken by whoever wrote the migration.

   **What the post and the screen show is this draft's own work** (#395). The path is the reads of the attempt being read — a message answered twice is two suggestions, each showing what its own draft did — and the questions of that attempt and the ones before it, because a question asked earlier is why this draft exists. The Gateway's own check of a proposed time is in the journal like every other read of the owner's calendar and is **not** in that list: it has its own line, and drawn there it read as the assistant looking at the same week twice. The clerk's post draws at most four steps and then counts the rest (`· et 8 autres lectures de votre agenda`), because the draft is what the owner decides about: measured on 2026-09-27, one post carried twelve path lines and the draft was the thirteenth.

   **And the day it names is checked like the hour** (#397). The fifth defect of this family, and the first about a *word*: on 2026-09-27 a draft offered a verified gap — the right instant, inside a window it had read, inside a real gap — and wrote *« mardi 12 »*. The 12th was a Monday, and a contact reads the word. So the Gateway compares the weekdays the sentence names against the days its instants fall on, in the owner's own time (the offset comes off the gaps' `start_local`, read and not computed), and refuses `hermes_answer_proposed_wrong_day` when not one of the named days is a day it offers. **Overlap, not agreement**: a reply may name a day it is declining — *"jeudi est pris, en revanche lundi 12…"* — as long as one named day is one it offers. A reply that names no weekday is untouched, and so is a deployment whose gaps carry no local time: refusing on a day computed in UTC would be this process inventing the kind of fact the check exists against.

   **And a draft that names a time says which gap it is, or it does not publish** (#383). This is the fourth defect of this path found by running it, and the one the other three lead to: with the zone right, the gaps handed over and the working day applied, a draft still offered two Monday hours from a week it had never read, both of them taken. Reading well is not the same as answering from what was read, and nothing between the two was checking. So the answer carries `proposed` — the instants the sentence offers, as RFC 3339 — and before anything is published the Gateway asks two questions of its own journal and of the calendar: was this instant inside a window that agent actually read for **this** message (`hermes_read`, joined on the `TWALK-REF` of #363), and is it still free (asked again, at that moment, and journalled under the same reference). A no to either is `422` and nothing on the bus: `hermes_answer_proposed_not_read`, `_not_free`, or `_uncheckable` when the check itself could not be made — because a proposal nobody verified is the thing the check exists against. A calendar that answers **no gaps at all** is `_uncheckable` and not "you are busy": the first is a sentence about the deployment and the second about the owner's week, and they must not be confused.

**A refused draft is a silence, and a silence is what you alert on.** Every one of these refusals is right, and every one of them costs the owner a message they never see: the contact wrote, the agent answered, and nothing reached the approval screen. That is the correct trade — a wrong hour or a wrong day is worse than a missing suggestion — but it is only correct while somebody notices. The counter is `twalk_companion_gateway_hermes_answers_total{outcome}`, one series per refusal code, and the rule worth having is:

```promql
increase(twalk_companion_gateway_hermes_answers_total{outcome=~"hermes_answer_proposed_.*"}[1h]) > 0
```

Any of that family climbing means drafts are being written and thrown away; which one says what to do next. `_not_read` and `_not_free`: the agent is offering times it did not read or that are taken, and the skill is what to tighten. `_wrong_day` (#397): it is naming the wrong weekday for a verified hour, and the answer is to say it louder in the skill — never to loosen the check, which is the only thing between a verified instant and a contact who writes the wrong day in their diary. `_uncheckable`: the deployment itself cannot answer, which is an operator's problem and not the agent's — look at the collector before you look at the prompt. A series that **does not exist** has never fired, which is the state a healthy deployment sits in.

And the other half of that ticket is the case it refuses to refuse. A reply may **name** an hour in its prose and list nothing, and refusing on a text pattern would refuse *"je te réponds sous 24h"*, which is not a proposal. So such a reply is published and **marked unverified**: `data.times` on the suggestion (`{"state": "checked", "count": n}` or `{"state": "unverified"}`, absent when the reply names no time), which the approval screen shows as a line and the clerk's post carries to Buzz. Both are drawn from the one member, asserted by the Gateway and never by an agent. What the owner reads is therefore either *the 2 times it offers were checked against your calendar* or *⚠️ this reply names a time nothing has verified* — and the difference between those two sentences is the whole of what this ticket bought.

   **Neither is a failure, and neither knowing is not one either.** A deployment whose calendars declare nothing and whose events are all in UTC gets all three members absent together, and the skill tells the agent to write its hours in UTC and say so — a sentence a person can act on, unlike an hour in the wrong zone, which reads perfectly and is wrong. Setting a zone on the calendar, in the calendar application, is what promotes it from `events` to `calendar`.

   **The same skill carries a second read since #355**, on the same secret and the same governance: `event-facts.sh <uid>` asks what one of your events *carries* — a join link when the event declares one in a property meant for it, how long its description is, how many attachments — and never what it says. Its journal is `hermes_event_read` and its counter `twalk_companion_gateway_hermes_event_reads_total{outcome}`, apart from the free/busy ones, because "how often was my agenda pulled" and "how often was an event asked about" are two questions you ask separately. No setting makes a description readable, the calendar-location switch (#354) included: that one governs the location and nothing else.

**Record of the first run — 2026-09-23, the reference deployment on `athena`.** The mail half is live; the calendar half is not, and why is the first thing this record has to say.

*Step 1, the grant.* The OIDC client is the one this owner's own mail sentinel already holds at LINAGORA's SSO (`sso.linagora.com`, a confidential client with `offline_access`), its secret in a file at 0600 on the host. Its only registered redirect URI belongs to another service of theirs, and using it worked: nothing there exchanges a code it did not ask for, so the browser kept the `code=` in its address bar and `provision-connection.sh` took it. The exchange with PKCE, the grant written at 0600, both whoamis printed — all on the first attempt.

**Two disagreements with the fake, both worth the ticket they cost.** The JMAP session answered `mmaudet@linagora.com` while `COLLECTOR_OWNER_EMAIL` said `michel.maudet@linagora.com`, so the grant was refused and removed — the check of [#274](https://github.com/linagora/twalk/issues/274) doing exactly its job, and a reminder that **the owner's address is the one the service answers**, not the one on a business card. The collector held one such address, so a mail from another alias of the owner's read as a contact's — fixed by [#322](https://github.com/linagora/twalk/issues/322): **`COLLECTOR_OWNER_ALIASES`** names every other address that is yours, the grant is yours if a service answers any of them (`provision-connection.sh` prints which one did), and an address that is yours and is not declared is a contact this collector will publish — a mail you sent from it waits for a decision about you, a decision about it is recorded like a stranger's, an event you attend under it has you withheld, a mail addressed to it alone is published as a group's rather than as direct, and a reply cannot leave at all if the mailbox sends as that address alone — a reply that does leave from one of these carries it as its `From`, and the approval's `posted-as` says so, because that header is what the reply was posted by. And the calendar side service this deployment points at is a **Cozy** instance, not the OpenPaaS one `caldav.rs` reads: `/api/user` answers `You must be authenticated` in `text/plain`, `OPTIONS /dav/` is `405`, and the host's own status endpoint answers a CouchDB health document. No scope or audience added to the OIDC client can change that — the collector reported it as `pending_operator` ("add an audience at the SSO"), which is the wrong instruction for this cause ([#320](https://github.com/linagora/twalk/issues/320)). So this deployment declares a **mail connection only**, and `authorize` says nothing about caldav at all since [#321](https://github.com/linagora/twalk/issues/321): a service no connection needs is no longer asked, so its refusal is no longer printed in the middle of a successful provisioning.

*Step 2, the service.* `docker compose --profile collector up -d --wait collector`, on a `.env` filled as this section says, gave the four lines the runbook asks for, in seconds:

```
the connection is connected                     connection="mail-linagora"
published fr.linagora.twalk.connection.status.changed.v1
push is on: a delivery wakes the mail poll      url=wss://jmap-new.linagora.com/jmap/ws
mailbox taken as it stands; nothing of it published
```

and the Companion Gateway, on the other side of the bus, `a connection changed state connection=mail-linagora from=unknown to=connected` — [#275](https://github.com/linagora/twalk/issues/275)'s seam, live, without anyone asking it anything.

**The line that matters most is the third.** TMail really serves RFC 8887 push with the ticket its session advertises, so [#277](https://github.com/linagora/twalk/issues/277)'s design — a socket that rings the poll — holds against the real server and not only against the fake: `twalk_collector_push_connected 1`, and 85 wakes in the first hour. Those wakes are the one thing the fake understated: TMail pushes on **every** `Email` state change, a flag or a move included, so most wakes find nothing to publish. The poll interval is what it is for the minutes the socket is down, and nothing more.

*Proof 1 — the grant is the owner's, and the owner is never a contact.* `provision-connection.sh` printed `jmap answers as mmaudet@linagora.com`; `GET /api/connections` and `twalk_collector_connection_state{connection="mail-linagora",state="connected"} 1` say the same afterwards. The owner-drop half was made the same morning: a mail the owner sent to their own address was read, recognised and dropped — `a mail was not published reason="owner"` in the log, `twalk_collector_events_dropped_total{reason="owner"}` at 1, no `inbound.message.received.v1` published for it, and the Gateway's pending-contact projection untouched. Neither the subject the owner chose nor their address appears anywhere in either log, which is the other half of what [ADR 0021](../docs/architecture/adr/0021-the-owner-is-never-a-contact-on-any-event.md) is for: the owner is not a contact, and a mail that is not published leaves no words behind.

*Proof 2 — a mail from a person is a message on the bus.* Made, and unprompted: a minute after the start, a real mail was published as `inbound.message.received.v1` (`twalk_collector_events_published_total{type="fr.linagora.twalk.inbound.message.received.v1"} 1`), and the Gateway projected its sender as a contact waiting for a decision. In the same hour and a half the frontier dropped **sixteen** mails as `non_human_sender` — fourteen automatic senders against one person, a ratio no fake ever produced and the best argument this project has for the frontier being worth its code. 

*The rest of proof 2, the same morning, and three walls.* The owner granted a colleague, she wrote, the persona drafted, and the approval was given from Buzz at 07:10 UTC. **The reply did not go out**, and neither of the two attempts after it did — each failure a thing the suites could not have caught, because in each case the fake was more forgiving than TMail:

1. **`the JMAP server answered unknownMethod`.** The batch that prepares a send carries `Identity/get`, a **submission** method (RFC 8621 §6), under the mail capability alone; RFC 8620 §3.2 makes a server answer `unknownMethod` to a call whose capability the request did not declare, and TMail does. The fake never read the `using` list ([#328](https://github.com/linagora/twalk/issues/328)).
2. **`the mail the reply answers is no longer in the mailbox`** — for a mail sitting unread in the owner's inbox. The thread is found by an `Email/query` filtered on `header: ["Message-ID", …]`, and **this server answers no header filter at all**: an empty list, which is exactly what an absent mail looks like. Both bracket forms were tried against the real service and both came back empty; the send now lists the mailbox's newest mails and matches Message-IDs itself ([#331](https://github.com/linagora/twalk/issues/331)).
3. **The reply left, and arrived blank.** The create carried the body as `bodyStructure` + `bodyValues`, which RFC 8621 §4.1.4 allows a client to send and which TMail does not read on create: it answered `created`, stored no body part, and submitted the result. No refusal, nothing in any log — the owner found it in their Sent folder ([#332](https://github.com/linagora/twalk/issues/332)).

With all three fixed and deployed, the reply left the owner's mailbox and was reported `reach=contact`, `posted-as: mailto:mmaudet@linagora.com`. **The proof this runbook asked for was not enough**: every log line and every metric was green on a message that said nothing. Proof 2 now ends by opening the sent mail and reading it, and that sentence is in the procedure above because of this run.

The double-send guard was found broken by the same measurement — it asks the same server the same kind of unanswerable question, about `X-Twalk-Approval`, a header of our own invention. It reads the Sent folder now, and a delivery of one approval twice sends one mail.

*Proofs 3 and 4 — the calendar, on 2026-09-24.* Made, once the right host was found — and finding it is the lesson. Three hosts were tried before this deployment read a calendar at all: `mmaudet-calendar.twake.linagora.com`, which is the **Cozy application** (no DAV under any path, `OPTIONS /dav/` → 405); `sabre-dav.twake-dev.maudet.cloud`, which is **sabre alone**, answering `/api/user` with a sabredav error document and refusing both spellings of the account; and `twcalendar.linagora.com`, which is the **OpenPaaS side service** this collector was written for — `/api/user` and a `/dav/` it relays, exactly the two routes `caldav.rs` asks for.

The credential turned out to be the one already in hand. The ESN accepts the same `sso.linagora.com` bearer the mailbox does, so the calendar is **the same connection's credential as the mail**: one collector, one grant, no second secret. Within seconds of the restart, `the connection is connected connection="calendar-linagora"`, `twalk_collector_connection_state{connection="calendar-linagora",state="connected"} 1`, and a free/busy read over a window in 2020 — chosen so the proof reads none of the owner's agenda — came back `a free/busy read was served intervals=0`, `twalk_collector_freebusy_reads_total{outcome="served"} 1`. That one read proves the whole chain: `/api/user` for the owner and their id, the HAL list of their calendars, and sabre's `free-busy-query` REPORT underneath.

What is still to run with a person in front of it is proof 3's other half — a meeting created with a granted and a revoked participant, to watch `participants_withheld` count the second — and proof 4's own half, a contact asking "are you free Thursday?" through Hermes.

**What an operator should take from this run.** The mail half works against the real TMail, push included — but not on the first afternoon: reading a mailbox worked immediately, and *answering* one took three fixes, each found by a person looking at a real mailbox rather than by a suite. The lesson is written into the tests rather than into a warning: where the fake cannot demonstrate that a real server accepts what it accepts, it refuses. The calendar half was written against an assumption about the side service that this deployment does not meet, and that is the kind of thing only a production run finds. Read `authorize`'s two whoami lines carefully: they are the cheapest place where a wrong address or an unreachable service says so.

**Upgrading the clerk past #311 takes three steps, and their order is the whole point.** Its `journal` channel now carries a line for a reply that **never left** — the Sensor or the collector gave up and dead-lettered it — as well as one for a reply that went out. A durable consumer keeps the configuration it was created with, so a clerk that already ran on this deployment keeps reading only the `.posted` subject until its durable is removed. Remove it with the clerk **stopped**:

```sh
docker compose stop clerk
nats consumer rm twalk clerk-journal
docker compose start clerk
```

Removing it while the clerk runs does not work, and fails in a way that looks like success: the running clerk notices its consumer has gone, rebuilds it within seconds — from **its own binary's** configuration, which on a not-yet-upgraded container is the single `.posted` subject — and the new container then adopts that durable and leaves it alone, because a durable that exists is returned as it is. Measured on the reference deployment on 2026-09-24, upgrading to #311: every container was new, every image was right, and the journal still read one subject.

So check rather than assume. `nats consumer info twalk clerk-journal` must answer two of them:

```
Filter Subjects: twalk.persona.reply.approved.v1.posted, twalk.persona.reply.approved.v1.dead
```

Nothing already on the relay is lost — the clerk holds no state of its own (ADR 0035) — but a report published while the durable is gone is one the journal will not carry, so do it while nothing is being approved.

## The search archive: an index you must encrypt before you turn it on

The collector can hold a **full-text index of the owner's archive** — their mail, then their messaging — so the Companion's `/search` can find a message from three years ago instead of the owner opening each client and remembering which of them has to say what (lot 3a). **What it does today, before you read further:** turning the index on makes searchable the mail the collector **reads from that moment on** — the live poll. The existing history is **not yet enumerated**: the archive-wide enumeration (JMAP `Email/query` paginated, resumable) arrives at **lot 3b**. Until then, a message from three years ago is not found because it has not been indexed at all. It is off unless you ask for it, and asking for it is one thing: a key file, named by `COLLECTOR_INDEX_KEY_FILE`. There is no other switch.

**It is the longest holder of other people's words in the whole deployment.** The bus keeps a contact's plaintext seven days ([ADR 0028](../docs/architecture/adr/0028-a-contacts-words-live-apart-from-the-event-that-identifies-them.md)); an archive of twenty-five years of mail keeps it for as long as the owner keeps the archive. That is the point — the archive is the owner's and they want it searchable — but it means the index is *the* place a stolen disk, a snapshot or a backup would yield the words, and the deployment is what has to make that costly. Twalk **names the requirement and cannot enforce it**: it writes the index under the state volume and refuses to open an index without a key, and it is you who puts that volume on an encrypted store ([ADR 0043](../docs/architecture/adr/0043-the-search-archive-is-the-only-long-term-holder-of-third-parties-words.md)).

**What the encryption protects, and what it does not.** It protects the bytes at rest: a disk lifted from the host, a filesystem snapshot, a backup taken while the collector is stopped. It does **not** protect a running process: while the collector runs, the decrypted index is mounted and readable by whoever can read the container's memory or the mount — an attacker already inside the deployment, or root on the host, sees plain text like the collector does. Encrypting at rest is not access control; it is what turns "the disk left the building" from a leak into a locked box. Say that plainly rather than letting "encrypted" read as "safe".

**Generate the key, once, off the repository.** The key is a passphrase file: it lives on this host at mode 0600, owned by you, and its *contents* never enter `.env`, the repository or a command line (the lesson of [#239](https://github.com/linagora/twalk/issues/239) — a secret on a `ps` line or in a committed file is a secret published). Keep a copy somewhere you would keep the archive's other keys, because the index is unreadable without it.

```bash
head -c 64 /dev/urandom > ~/deploy/twalk-secrets/collector-index-key
chmod 0600 ~/deploy/twalk-secrets/collector-index-key
```

**Then make the index sit on ciphertext.** The index lives at `/data/index`, inside the `collector-data` volume — the same volume as the grant and the cursors — so there is no second directory to mount: you encrypt the volume's backing store, and everything under `/data` is on ciphertext at rest. The clean way is to back the volume with a **gocryptfs** mount instead of a plain directory:

```bash
mkdir -p /mnt/twalk-archive/cipher /mnt/twalk-archive/plain
gocryptfs -init -passfile ~/deploy/twalk-secrets/collector-index-key /mnt/twalk-archive/cipher
gocryptfs -passfile ~/deploy/twalk-secrets/collector-index-key /mnt/twalk-archive/cipher /mnt/twalk-archive/plain
```

Then point the service's `/data` at the **decrypted mount point** with a compose override, kept out of git so the manifest stays the plain deployment. The named `collector-data` volume is simply not used once `/data` is bound to a host path:

```yaml
# docker-compose.override.yml — the encrypted backing store for the archive
services:
  collector:
    volumes:
      - /mnt/twalk-archive/plain:/data
```

The container mounts `/mnt/twalk-archive/plain` and sees below it a normal directory; the disk, under `/mnt/twalk-archive/cipher`, holds only ciphertext. The gocryptfs mount must be up **before** the container starts, or the collector writes an index onto the unencrypted mount point — an init service, an `fstab` entry with the passfile, or a start-up script is where that belongs. A deployment that already encrypts its Docker data root, or that runs the whole host on an encrypted filesystem, satisfies the requirement with no override at all — what matters is that the bytes on the platter are not the owner's mail.

**Name the key and bring the collector up.** One line in `docker-compose/.env`:

```bash
COLLECTOR_INDEX_KEY_FILE=/home/<operator>/deploy/twalk-secrets/collector-index-key
```

The container mounts that file read-only, the entrypoint copies it for the unprivileged `collector` account at 0600 and exports the path, and the collector opens the index on the decrypted mount. Unset, the index stays off and `/search` answers `503 index_not_configured` — which is a legitimate state and not an error, so the collector starts either way: a missing key is a capability not asked for, never a failure of the mail. `COLLECTOR_INDEX_KEY_FILE` names a **file on the host**; its contents are an operator secret and are never committed, exactly like `token-or.env`.

**Verify the disk sees only ciphertext.** Stop nothing; just look under the cipher directory for what the collector has been writing, and confirm it is not readable mail:

```bash
ls /mnt/twalk-archive/cipher          # gocryptfs's own layout: gocryptfs.conf, gocryptfs.diriv, and opaque names
gocryptfs -passfile ~/deploy/twalk-secrets/collector-index-key -info /mnt/twalk-archive/cipher
strings /mnt/twalk-archive/cipher/* 2>/dev/null | grep -i subject | head   # nothing: the disk holds no plaintext
```

If the third command prints a subject you recognise, the index is not on the encrypted store and the mount is wrong — stop the collector, fix the override, and start it again. The proof is the same shape as the other production proofs in this runbook: not "encryption is configured" but "the disk was read and held no words".

## Publishing the Companion: two lines, and one that must stay unpublished

The Companion is a browser application, so publishing it means publishing the **homeserver** too — and that is the part a deployment gets wrong silently. The browser signs a device in with Matrix OpenID (ADR 0011): it asks the owner's homeserver for a token and posts it to the Gateway. To find that homeserver it used the deployment's **server name**, turned into `https://<server name>`.

A server name is not always an address the outside can call, and it cannot be changed: it is in every user id, room id and device a live deployment has. This one is the case — `MATRIX_DOMAIN=twalk.localhost`, Synapse on loopback, and the Companion published behind the operator's SSO at another name entirely. A browser there resolved `twalk.localhost` to **its own** machine. What that looked like to the owner, on 2026-10-04, was the recovery screen answering `the login response was not a session` — their own laptop replying to a password login (#323).

So, two lines that go together:

1. **In your reverse proxy**, put `/_matrix/` in front of Synapse on the **same** public name the Companion is served under, so the browser's Matrix requests are same-origin:

   ```
   companion.example.com/_matrix/   →  synapse:8008/_matrix/
   companion.example.com/           →  companion-gateway:8080
   ```

2. **In `.env`**, tell the deployment that address, with no trailing slash:

   ```
   GATEWAY_HOMESERVER_CLIENT_URL=https://companion.example.com
   ```

`GET /api/deployment` then carries it as `client_url`, the sign-in and recovery screens call that address instead of deriving one from the name, and **nothing server-side changes**: the Gateway still verifies the OpenID token against `GATEWAY_HOMESERVER_FEDERATION_URL` inside the deployment, and still checks the user is `GATEWAY_OWNER`. Unset, a deployment whose name *is* its address behaves exactly as before.

Two paths must be left out of what you publish. **`/_twalk/*`** carries its own credentials — a bridge's `as_token`, the secret shared with Hermes — and belongs on the deployment's own network. And **`/_synapse/admin`** is not among the upstreams above at all, which is the point of listing `synapse:8008/_matrix/` rather than `synapse:8008`.

### An SSO in front of the Companion does not go in front of Matrix

If your proxy authenticates the Companion — an SSO, a basic-auth gate, anything that answers a stranger with a redirect — then the **whole** client-server API has to be exempt from it. Not the handful of routes a browser calls before it has a session: all of it.

**No Matrix client sends cookies.** matrix-js-sdk hardcodes it, and the Companion's own password login does the same, for the same reason:

```js
credentials: "omit",   // we send credentials via headers
```

A request that authenticates with a bearer token cannot pass a cookie gate — before a sign-in or after one. An earlier version of this section said otherwise, that the login endpoint and the rest could stay behind the SSO; what that cost, on 2026-10-04, was the owner of the deployment described above locked out of their own keys (#448):

```text
POST /_matrix/client/v3/login   →  302 to the SSO
fetch follows the redirect      →  200 text/html
the recovery screen reads       →  `the login response was not a session`
```

With oauth2-proxy, that is one line:

```
OAUTH2_PROXY_SKIP_AUTH_ROUTES: "^/_matrix/,^/.well-known/matrix/"
```

What guards the API instead is Matrix's own, which is how every homeserver on the federation is published. Every route but those needs an access token Synapse issued — `/keys/query` with none answers `M_MISSING_TOKEN`. Registration is closed on this deployment (`synapse/homeserver.yaml` sets no `enable_registration`; accounts come from `provision.sh`), so `POST /_matrix/client/v3/register` answers `M_FORBIDDEN`. And `rc_login.failed_attempts` — 0.2/s with a burst of 10 — is the limiter that carries the brute-force argument. The SSO still guards what it was there for: the Companion and the Gateway's `/api`.

One command says which side of this a deployment is on, and you can run it from anywhere:

```sh
curl -s -o /dev/null -w '%{http_code} %{content_type}\n' \
  -XPOST -H 'Content-Type: application/json' \
  -d '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"nobody"},"password":"x"}' \
  https://companion.example.com/_matrix/client/v3/login

# 403 application/json  → the homeserver answered. A browser can sign in.
# 200 text/html         → your gate answered. It cannot, and the screen
#                         will blame the login response.
```

Two things that are *not* enough on their own: being signed into the SSO in your browser (the cookie exists, the request just does not carry it), and `/_matrix/client/versions` answering 200 (that route is usually the first one an operator exempts, so it answers while everything after it does not).

## What is where

| Path | What it is |
| --- | --- |
| `docker-compose/compose.yaml` | The stack. Bridges behind `profiles: [bridges]`, the collector behind `profiles: [collector]` |
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
