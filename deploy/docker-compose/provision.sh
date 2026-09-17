#!/usr/bin/env bash
# Provision Matrix accounts on the reference deployment's Synapse. Open
# registration is off; register_new_matrix_user authenticates with the
# registration shared secret from the env file (rendered into the Synapse
# config).
#
#   ./provision.sh                  provision the Sensor account
#                                   (SENSOR_USER_ID / SENSOR_PASSWORD in .env)
#   ./provision.sh <login> <pass>   provision another account
#
# Idempotent: an account that already exists is skipped, not overwritten.
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

if [ $# -eq 0 ]; then
  user_id="$(env_value SENSOR_USER_ID)"
  password="$(env_value SENSOR_PASSWORD)"
  if [ -z "$user_id" ] || [ -z "$password" ]; then
    echo "SENSOR_USER_ID and SENSOR_PASSWORD must be set in $ENV_FILE" >&2
    exit 1
  fi
  # @sensor:example.com -> sensor
  localpart="${user_id#@}"
  localpart="${localpart%%:*}"
elif [ $# -eq 2 ]; then
  localpart="$1"
  password="$2"
else
  echo "usage: $0 [localpart password]" >&2
  exit 2
fi

output=$(docker compose -p "$PROJECT" --env-file "$ENV_FILE" -f "$DEPLOY_DIR/compose.yaml" \
  exec -T synapse register_new_matrix_user \
    -u "$localpart" -p "$password" --no-admin \
    -c /data/homeserver.yaml http://localhost:8008 2>&1) || true
if grep -q "Success" <<<"$output"; then
  echo "provisioned $localpart"
elif grep -qi "already" <<<"$output"; then
  echo "$localpart already exists, skipped"
else
  echo "failed to provision $localpart:" >&2
  echo "$output" >&2
  exit 1
fi
