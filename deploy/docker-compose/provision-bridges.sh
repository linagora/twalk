#!/usr/bin/env bash
# Provision the mautrix bridges of the reference deployment: generate each
# bridge's appservice registration, install it in Synapse's configuration, and
# restart Synapse so it reads it.
#
#   ./provision-bridges.sh                  whatsapp, signal and gmessages
#   ./provision-bridges.sh whatsapp         one of them only
#   ./provision-bridges.sh gmessages        the SMS bridge on its own
#
# The names are mautrix's own names for the bridges, and each one selects that
# bridge's binary, appservice id and registration file. `gmessages` is a bridge id
# and never a network value (ADR 0005): the network it serves is `sms`, whether
# it transits Google Messages Web or, in v0.2, the sovereign SMS Companion.
#
# Then bring the stack up with the profile on:
#
#   docker compose --profile bridges up -d --wait
#
# Why this is a separate step and not part of `docker compose up`: a bridge
# generates its own registration (`mautrix-<id> -g`), but *installing*
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

KNOWN_BRIDGES=(whatsapp signal gmessages)

bridges=("$@")
if [ ${#bridges[@]} -eq 0 ]; then
  bridges=("${KNOWN_BRIDGES[@]}")
fi
for bridge in "${bridges[@]}"; do
  case " ${KNOWN_BRIDGES[*]} " in
    *" $bridge "*) ;;
    *)
      echo "unknown bridge: $bridge (known: ${KNOWN_BRIDGES[*]})" >&2
      echo "these are mautrix's own names for the bridges this deployment ships," >&2
      echo "not network names: the SMS bridge is 'gmessages' (ADR 0005), and" >&2
      echo "'sms' is the network it serves rather than a name to pass here." >&2
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
for bridge in "${bridges[@]}"; do
  expected="${expected:+$expected,}/registrations/$bridge.yaml"
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
for bridge in "${bridges[@]}"; do
  echo "==> generating the $bridge bridge's registration"
  compose run --rm --no-deps -T "bridge-$bridge-registration"
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

Bridges provisioned: ${bridges[*]}

Next:
  docker compose --profile bridges up -d --wait

Then a human finishes the job: every login needs the phone or the account that
holds it, and no script can stand in. WhatsApp and Signal are a QR code to
scan. SMS through Google Messages is seven session cookies copied out of a
PRIVATE browsing window of your own Google account, followed by an emoji match
on the phone — nothing here asks for those cookies, reads them or stores them,
and driving a browser to harvest them was rejected outright (#57).

The Companion's networks screens exist for exactly this, and until they land
the login runs through each bridge's provisioning API or its management room.
EOF
