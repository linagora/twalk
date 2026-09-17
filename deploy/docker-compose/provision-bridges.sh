#!/usr/bin/env bash
# Provision the mautrix bridges of the reference deployment: generate each
# bridge's appservice registration, install it in Synapse's configuration, and
# restart Synapse so it reads it.
#
#   ./provision-bridges.sh                  whatsapp and signal
#   ./provision-bridges.sh whatsapp         one of them only
#
# Then bring the stack up with the profile on:
#
#   docker compose --profile bridges up -d --wait
#
# Why this is a separate step and not part of `docker compose up`: a bridge
# generates its own registration (`mautrix-<network> -g`), but *installing*
# one means naming its path in the homeserver's `app_service_config_files` and
# restarting the homeserver. Synapse has no runtime API for that, so no
# running service can do it — which is also why the Companion Gateway's bridge
# facade (#47) deliberately owns no registration. Registration generation
# itself needs no homeserver, so this script is safe to run before the stack
# has ever started, and again on a stack that is already up.
#
# Idempotent: with an unchanged .env the generated registration is
# byte-identical, so a second run changes nothing Synapse has to notice.
#
# TWALK_COMPOSE_PROJECT overrides the compose project (default: twalk);
# TWALK_ENV_FILE overrides the env file (default: .env next to this script).
set -euo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
ENV_FILE="${TWALK_ENV_FILE:-$DEPLOY_DIR/.env}"
PROJECT="${TWALK_COMPOSE_PROJECT:-twalk}"

if [ ! -f "$ENV_FILE" ]; then
  echo "env file not found: $ENV_FILE (cp .env.example .env and edit it first)" >&2
  exit 1
fi

# Reads one KEY=value line from the env file without executing it.
env_value() {
  grep -E "^$1=" "$ENV_FILE" | tail -1 | cut -d= -f2-
}

compose() {
  docker compose -p "$PROJECT" --env-file "$ENV_FILE" \
    -f "$DEPLOY_DIR/compose.yaml" --profile bridges "$@"
}

networks=("$@")
if [ ${#networks[@]} -eq 0 ]; then
  networks=(whatsapp signal)
fi
for network in "${networks[@]}"; do
  case "$network" in
    whatsapp | signal) ;;
    *)
      echo "unknown bridge network: $network (known: whatsapp, signal)" >&2
      exit 2
      ;;
  esac
done

# Synapse reads exactly the paths MATRIX_APPSERVICE_REGISTRATIONS names, and
# refuses to start if one of them does not exist — so the list and the set of
# provisioned bridges have to agree exactly. Checking it here, before anything
# is generated, is the difference between one clear message and a homeserver
# that will not boot.
expected=""
for network in "${networks[@]}"; do
  expected="${expected:+$expected,}/registrations/$network.yaml"
done
configured="$(env_value MATRIX_APPSERVICE_REGISTRATIONS | tr -d '[:space:]')"
normalize() { tr ',' '\n' <<<"$1" | sed '/^$/d' | sort; }
if [ "$(normalize "$configured")" != "$(normalize "$expected")" ]; then
  cat >&2 <<EOF
MATRIX_APPSERVICE_REGISTRATIONS in $ENV_FILE does not match the bridges being
provisioned.

  it says:      ${configured:-(empty)}
  it must say:  $expected

Set that line and run this script again. Synapse reads these paths from its
configuration at startup and fails on one that does not exist, so the list
must name every bridge you provision and no other.
EOF
  exit 1
fi

# 1. Each bridge's own registration. `run --rm` rather than `up`, so that this
#    works on a stack that has never been started: generating a registration
#    talks to nothing.
for network in "${networks[@]}"; do
  echo "==> generating the $network bridge's registration"
  compose run --rm --no-deps -T "bridge-$network-registration"
done

# 2. Re-render Synapse's configuration, which is where the registrations are
#    installed. On a fresh stack this also generates the signing key, exactly
#    as the synapse-config one-shot does on the way up.
echo "==> rendering Synapse's configuration with the registrations installed"
compose run --rm --no-deps -T synapse-config >/dev/null

# 3. The restart Synapse needs to read them. Only if it is running: on a fresh
#    stack the operator's `up` starts it with the configuration already
#    rendered, and there is nothing to restart.
synapse_state="$(compose ps -a --format json synapse 2>/dev/null |
  sed -n 's/.*"State":"\([^"]*\)".*/\1/p' | head -1)"
if [ "$synapse_state" = "running" ]; then
  echo "==> restarting Synapse so it reads the registrations"
  compose restart synapse
  for _ in $(seq 60); do
    health="$(compose ps --format json synapse 2>/dev/null |
      sed -n 's/.*"Health":"\([^"]*\)".*/\1/p' | head -1)"
    [ "$health" = "healthy" ] && break
    sleep 1
  done
  if [ "${health:-}" != "healthy" ]; then
    echo "Synapse did not become healthy again after the restart; check" >&2
    echo "  docker compose -p $PROJECT logs synapse" >&2
    exit 1
  fi
else
  echo "==> Synapse is not running; its configuration is ready for the next start"
fi

cat <<EOF

Bridges provisioned: ${networks[*]}

Next:
  docker compose --profile bridges up -d --wait

Then a human finishes the job: each network's login needs a phone. The
Companion's networks screens exist for exactly that, and until they land the
login runs through the bridge's provisioning API or its management room — a
QR code to scan (WhatsApp, Signal) that no script can scan for you.
EOF
