#!/usr/bin/env bash
# Give the collector its OIDC grant: you sign in at your SSO, once, at this
# terminal, and the collector holds a refresh token for your account from
# then on (ADR 0033, #274).
#
#   ./provision-connection.sh [--renew] [env-file]      default: ./.env
#
# This is the operator route, in the shape of provision-owner-device.sh:
# the one interactive step a deployment of the collector has. It runs the
# collector's own `authorize` command inside the collector's container, with
# the stack's .env, so the grant lands where the running collector reads it
# (the collector-data volume, `oidc/grant.json`, mode 0600) and nothing has
# to be copied afterwards.
#
# What happens, in order: the collector prints a link; you open it in a
# browser and sign in as COLLECTOR_OWNER_EMAIL; the SSO redirects the browser
# to COLLECTOR_OIDC_REDIRECT_URI, where nothing listens; you copy the whole
# address from the address bar and paste it here. The collector exchanges the
# code with PKCE, refuses a grant that came with no refresh token, writes the
# grant, and asks both services who the token belongs to — printing what they
# answered, and never a token. A grant for another account is refused there
# and then.
#
# Idempotent: a grant already in the volume is left alone and this script
# says so; --renew replaces it explicitly — what you do after revoking the
# grant at the SSO, or when the collector says `reconnect_required` on the
# bus.
set -euo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
RENEW=
if [ "${1:-}" = "--renew" ]; then
  RENEW=--renew
  shift
fi
ENV_FILE="${1:-${TWALK_ENV_FILE:-$DEPLOY_DIR/.env}}"
[ -f "$ENV_FILE" ] || { echo "env file not found: $ENV_FILE" >&2; exit 1; }

env_value() { grep -E "^$1=" "$ENV_FILE" | tail -1 | cut -d= -f2-; }

for name in COLLECTOR_OIDC_ISSUER COLLECTOR_OIDC_CLIENT_ID COLLECTOR_OIDC_CLIENT_SECRET_FILE \
            COLLECTOR_JMAP_SESSION_URL COLLECTOR_CALDAV_URL COLLECTOR_OWNER_EMAIL; do
  if [ -z "$(env_value "$name")" ]; then
    echo "$name is empty in $ENV_FILE: see deploy/docker-compose/.env.example, the collector section" >&2
    exit 1
  fi
done
if [ -z "$(env_value COLLECTOR_MAIL_CONNECTION)" ] && [ -z "$(env_value COLLECTOR_CALENDAR_CONNECTION)" ]; then
  echo "neither COLLECTOR_MAIL_CONNECTION nor COLLECTOR_CALENDAR_CONNECTION is set in $ENV_FILE:" >&2
  echo "the collector would hold no connection. Declare one in GATEWAY_CONNECTIONS and name it here." >&2
  exit 1
fi
secret_file="$(env_value COLLECTOR_OIDC_CLIENT_SECRET_FILE)"
if [ ! -s "$secret_file" ]; then
  echo "COLLECTOR_OIDC_CLIENT_SECRET_FILE names $secret_file, which is missing or empty." >&2
  exit 1
fi

# The collector's own authorize command, in its own container, with the
# stack's .env and stdin attached for the one line you paste. No token is
# printed by it, and none is passed on a command line here.
cd "$DEPLOY_DIR"
exec docker compose --env-file "$ENV_FILE" --profile collector run --rm -i collector authorize $RENEW
