# deploy

Deployment manifests: Docker Compose (the v0.1 reference path), Kubernetes overlays (v0.2), and bare-metal Ansible.

`docker-compose/` is the reference deployment: Synapse (the Matrix hub), NATS JetStream (the bus), the Sensor, the Companion Gateway, and — behind a compose profile — the `mautrix-whatsapp` and `mautrix-signal` bridges. Everything is configured through one environment file; copy `docker-compose/.env.example` to `.env` and edit it first. That file documents every variable and is the place to read before this one.

## The stack without bridges

```bash
cd docker-compose
cp .env.example .env    # then edit it
docker compose up -d --wait
```

One step, no manual provisioning: the stack's `provision` one-shot creates the Sensor account on the way up. `./provision.sh` remains for ad-hoc accounts.

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

If you skip step 1, the symptom is specific: the bridge container crash-loops with an appservice authentication error in its log, because it asserts its own registration against the homeserver at startup and Synapse has never heard of it.

### What is still a human's job

Connecting a network is a human act and stays one. WhatsApp and Signal are both paired by scanning a QR code with the phone that holds the account: no script can scan it, and the deployment test does not try. Once the bridges are up, the login runs through each bridge's provisioning API — which is what the Companion's networks screens ([#68](https://github.com/linagora/twalk/issues/68)) and the Companion Gateway's facade ([#55](https://github.com/linagora/twalk/issues/55)) exist to put in front of a human — or, until those land, through the bridge's Matrix management room (`!wa login`, `!signal login`) from your own Matrix client.

For those portal rooms to reach the bus, `SENSOR_ALLOWED_INVITERS` has to name each bridge's bot (`@whatsappbot:<domain>`, `@signalbot:<domain>`). See `.env.example`.

## What is where

| Path | What it is |
| --- | --- |
| `docker-compose/compose.yaml` | The stack. Bridges behind `profiles: [bridges]` |
| `docker-compose/.env.example` | Every variable, documented; the file to read first |
| `docker-compose/provision.sh` | Matrix account provisioning (the Sensor, ad-hoc accounts) |
| `docker-compose/provision-bridges.sh` | Step 1 above: registrations, Synapse's configuration, the restart |
| `docker-compose/synapse/homeserver.yaml` | Synapse's configuration template (Jinja2, rendered by the image) |
| `docker-compose/*.Dockerfile` | One image per Twalk component |
| `../bridges/` | Each bridge's base configuration, and the generator the one-shots run |

Covered end to end by `sensor/tests/deployment.rs` (the pipeline, bridges off), `sensor/tests/bridges_deployment.rs` (the two-step procedure above, bridges on) and `companion-gateway/tests/deployment.rs` (the Companion's origin).
