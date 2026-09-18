# deploy

Deployment manifests: Docker Compose (the v0.1 reference path), Kubernetes overlays (v0.2), and bare-metal Ansible.

`docker-compose/` is the reference deployment: Synapse (the Matrix hub), NATS JetStream (the bus), the Sensor, the Companion Gateway, Hermes with the personas it hosts, and — behind a compose profile — the `mautrix-whatsapp` and `mautrix-signal` bridges. Everything is configured through one environment file; copy `docker-compose/.env.example` to `.env` and edit it first. That file documents every variable and is the place to read before this one.

## The stack without bridges

```bash
cd docker-compose
cp .env.example .env    # then edit it
docker compose up -d --wait
```

One step, no manual provisioning: the stack's `provision` one-shot creates the Sensor account on the way up. `./provision.sh` remains for ad-hoc accounts.

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
HERMES_LLM_PARAMS={"max_tokens":2000}           # a reasoning model needs the room to think
```

That last line is not decoration. The `assistant` persona asks for 300 tokens; a model that reasons before it answers spends all of them thinking, and the answer comes back with no content at all — the persona then logs `the chat-completions answer carries no content` and retries for ever, with the endpoint answering `200` every time. It is the first thing to check when a persona is running, reaching the model, and producing nothing — and that the persona cannot tell you which of the two happened is [#162](https://github.com/linagora/twalk/issues/162).

The cost of that namespace is that Hermes and every persona can reach anything bound to this host's loopback. If your endpoint is reachable at a routable address instead, ADR 0023's last section says what to change to put Hermes back on the stack's own network.

Which persona runs is still the user's decision and not this file's: a persona nobody activated is paused, receives nothing, and keeps running (`ADR 0013`). Activation happens on the Companion's `/personas` screen.

## The stack with bridges: two steps, and why

```bash
./provision-bridges.sh                         # 1. registrations, then Synapse
docker compose --profile bridges up -d --wait  # 2. the stack, bridges included
```

**The second step alone does not work, and this is not a convenience we removed — it is a property of Matrix.** A bridge joins a homeserver as an *appservice*, and an appservice is installed by writing its registration file, naming that file in the homeserver's `app_service_config_files`, and restarting the homeserver. Synapse has no API for registering an appservice at runtime; neither does any other homeserver. So nothing that is already running can install a bridge — not the bridge, not the Companion Gateway, not a compose dependency. The bridge facade spec ([#47](https://github.com/linagora/twalk/issues/47)) draws the line in the same place: the Gateway drives logins and reads status, and owns no registration.

What each step does:

1. `./provision-bridges.sh` runs each bridge's own `mautrix-<network> -g` to generate its registration into a volume Synapse reads, re-renders Synapse's configuration with those paths installed, and restarts Synapse if it is running. Generating a registration talks to no homeserver, so this works on a stack that has never started, and it is idempotent: with an unchanged `.env` the registration is byte-identical and Synapse has nothing to notice. Take `./provision-bridges.sh whatsapp` to provision one bridge only.
2. `docker compose --profile bridges up -d --wait` brings the bridges up. Without `--profile bridges` they are ignored entirely — an operator who does not want them pulls no bridge image, runs no bridge container and sets no bridge variable.

One line in `.env` ties the two together and `provision-bridges.sh` checks it before it generates anything: `MATRIX_APPSERVICE_REGISTRATIONS` must name exactly the bridges you provision. Synapse refuses to start on a registration path that does not exist, so a list that is ahead of reality is a homeserver that will not boot.

If you skip step 1 the failure is loud but its shape depends on what `.env` says, so both are worth recognising. With `MATRIX_APPSERVICE_REGISTRATIONS` still empty, Synapse comes up happily and each bridge crash-loops instead, with an appservice authentication error in its log: it asserts its own registration against the homeserver at startup, and Synapse has never heard of it. With the line already set, Synapse itself may refuse to start, because the registration path it is told to read does not exist yet — `up` does start the registration one-shots, but nothing orders them before Synapse, and that ordering is not something a compose dependency can express without dragging the whole profile back into a bridgeless stack. Either way the fix is the same: run step 1.

### What is still a human's job

Connecting a network is a human act and stays one. WhatsApp and Signal are both paired by scanning a QR code with the phone that holds the account: no script can scan it, and the deployment test does not try. Once the bridges are up, the login runs through each bridge's provisioning API — which is what the Companion's networks screens ([#68](https://github.com/linagora/twalk/issues/68)) and the Companion Gateway's facade ([#55](https://github.com/linagora/twalk/issues/55)) exist to put in front of a human — or, until those land, through the bridge's Matrix management room (`!wa login`, `!signal login`) from your own Matrix client.

### Choosing which conversations are observed

A bridge builds a portal room **when a conversation becomes active**, not once at login. On the reference deployment one WhatsApp account produced eighteen of them over a single day, as people wrote — and mautrix invites only *the user* into each. So a connected network does not put anything on the bus by itself, and the set of conversations keeps growing for as long as the deployment runs.

`SENSOR_ALLOWED_INVITERS` is necessary and is not sufficient. It has to name each bridge's bot (`@whatsappbot:<domain>`, `@signalbot:<domain>`) — that is what lets the Sensor accept an invitation — but it settles only what the Sensor *accepts*, and nothing in mautrix ever asks. What completes the path is the **portal register** ([#105](https://github.com/linagora/twalk/issues/105)): the Companion Gateway reads each bridge's portal rooms as that bridge's own bot, using `GATEWAY_BRIDGE_<ID>_AS_TOKEN`, and invites the Sensor into the conversations the user chooses.

Three things follow, and each is worth knowing before you go looking for a missing message.

- **Nothing is observed by default, and that is deliberate.** Those eighteen rooms held roughly 1,300 memberships, several hundred people who do not know Twalk exists. Observation is chosen per conversation, and until it is chosen the Sensor is in none of them.
- **The deployment can say how many conversations it is outside.** `GET /api/portals` lists every conversation with its name, its size and whether the Sensor is inside; `/metrics` carries the same counts as `twalk_companion_gateway_portal_rooms{observation="observing|invited|absent"}`. The Sensor's own `twalk_sensor_observed_rooms` says how many rooms it is actually reading.
- **A conversation stuck at `invited` is a configuration error with a name.** The Gateway invited the Sensor and the Sensor refused the inviter: that bridge's bot is missing from `SENSOR_ALLOWED_INVITERS`. The Sensor counts those refusals as `twalk_sensor_invites_total{outcome="ignored"}` and logs one warning each.

A bridge whose `GATEWAY_BRIDGE_<ID>_AS_TOKEN` is unset has none of its conversations read at all, and says so in `GET /api/portals` rather than quietly contributing nothing to the totals. `GATEWAY_PORTAL_REFRESH_SECONDS` decides only how fresh the `/metrics` counts are (300 by default, `0` turns the background read off); the API always reads the homeserver there and then.

## What is where

| Path | What it is |
| --- | --- |
| `docker-compose/compose.yaml` | The stack. Bridges behind `profiles: [bridges]` |
| `docker-compose/.env.example` | Every variable, documented; the file to read first |
| `docker-compose/provision.sh` | Matrix account provisioning (the Sensor, ad-hoc accounts) |
| `docker-compose/provision-bridges.sh` | Step 1 above: registrations, Synapse's configuration, the restart |
| `docker-compose/synapse/homeserver.yaml` | Synapse's configuration template (Jinja2, rendered by the image) |
| `docker-compose/*.Dockerfile` | One image per Twalk component |
| `docker-compose/hermes-entrypoint.sh` | Hermes's entrypoint: start the runtime, or say why it is hosting nothing |
| `docker-compose/run-persona-image.sh` | The argv every persona in `HERMES_PERSONAS` names, shipped in the Hermes image as `run-persona` |
| `../bridges/` | Each bridge's base configuration, and the generator the one-shots run |

Covered end to end by `sensor/tests/deployment.rs` (the pipeline, bridges off), `sensor/tests/bridges_deployment.rs` (the two-step procedure above, bridges on), `companion-gateway/tests/deployment.rs` (the Companion's origin) and `hermes/tests/full_loop.rs` (the whole loop, on this stack's own Hermes).
