#!/usr/bin/env bash
# Render a mautrix bridge's configuration and generate its appservice
# registration. Runs inside the bridge's own image (which ships bash and yq),
# as the `bridge-<id>-registration` one-shot of
# deploy/docker-compose/compose.yaml.
#
# BRIDGE_MAUTRIX_ID is mautrix's own name for the bridge — `whatsapp`,
# `signal`, `gmessages` — which is what selects the binary, the appservice id
# and the registration's filename. It is deliberately NOT called a network:
# `gmessages` is a bridge and never a network value (ADR 0005), and the
# network this bridge serves (`sms`) is the Companion Gateway's business, not
# this script's.
#
# Why this is provisioning and not a runtime concern: a registration is
# installed by listing its path in the homeserver's `app_service_config_files`
# and restarting the homeserver. Synapse has no runtime API for it, so nothing
# a running service does can install one — see the bridge facade spec (#47),
# which keeps registration generation out of the Companion Gateway for exactly
# this reason.
#
# What it does, in order:
#   1. copies the committed base config (bridges/mautrix-<id>/config.yaml)
#      over /data/config.yaml, so the operator's .env and this repository are
#      the only sources of the bridge's configuration;
#   2. renders the environment-dependent fields onto it with yq;
#   3. runs `mautrix-<id> -g`, which is what builds a valid registration
#      (its id, url, namespaces and the MSC2409 flags);
#   4. pins the two appservice tokens and the sender localpart back to the
#      values the operator chose, because `-g` regenerates all three on every
#      run. Pinning is what makes this script idempotent: run twice with the
#      same .env and the registration is byte-identical, so Synapse needs no
#      restart.
#
# Idempotent, and safe to run against a stack that is already up: it touches
# the bridge's config and registration only. Installing the registration into
# Synapse (and restarting it) is deploy/docker-compose/provision-bridges.sh's
# job.
set -euo pipefail

BASE_CONFIG=${BASE_CONFIG:-/opt/twalk/config.yaml}
DATA_DIR=${DATA_DIR:-/data}
REGISTRATION_DIR=${REGISTRATION_DIR:-/registrations}

die() {
  echo "generate-registration: $*" >&2
  exit 1
}

require() {
  local name=$1
  [ -n "${!name:-}" ] || die "$name must be set (see deploy/docker-compose/.env.example)"
}

require BRIDGE_MAUTRIX_ID
require MATRIX_DOMAIN
require MATRIX_INTERNAL_URL
require MATRIX_OWNER_USER_ID
require BRIDGE_AS_TOKEN
require BRIDGE_HS_TOKEN
require BRIDGE_PROVISIONING_SECRET

binary="/usr/bin/mautrix-${BRIDGE_MAUTRIX_ID}"
[ -x "$binary" ] || die "$binary not found: BRIDGE_MAUTRIX_ID=$BRIDGE_MAUTRIX_ID does not match this image"
[ -f "$BASE_CONFIG" ] || die "base config $BASE_CONFIG not mounted"

# mautrix answers M_FORBIDDEN to the entire provisioning API when the shared
# secret is shorter than 16 characters, and says nothing about why — so refuse
# it here, where the message can be read.
if [ "${#BRIDGE_PROVISIONING_SECRET}" -lt 16 ]; then
  die "the provisioning shared secret must be at least 16 characters (mautrix rejects every provisioning request otherwise); got ${#BRIDGE_PROVISIONING_SECRET}"
fi

config="$DATA_DIR/config.yaml"
registration="$DATA_DIR/registration.yaml"
installed="$REGISTRATION_DIR/${BRIDGE_MAUTRIX_ID}.yaml"

mkdir -p "$DATA_DIR" "$REGISTRATION_DIR"

# `-g` randomises the sender localpart on every run. Keep the one already
# installed, so that re-provisioning does not leave an orphaned appservice
# sender behind on the homeserver and does not change the file Synapse reads.
sender_localpart=""
if [ -f "$installed" ]; then
  sender_localpart=$(yq e '.sender_localpart // ""' "$installed")
fi

cp "$BASE_CONFIG" "$config"

# The environment-dependent half of the configuration. `strenv` keeps every
# value a string, so a token that looks like a number stays a token. An empty
# BRIDGE_STATUS_ENDPOINT renders as an empty value, which is how mautrix
# spells "push status nowhere".
MATRIX_DOMAIN="$MATRIX_DOMAIN" \
MATRIX_INTERNAL_URL="$MATRIX_INTERNAL_URL" \
MATRIX_OWNER_USER_ID="$MATRIX_OWNER_USER_ID" \
BRIDGE_AS_TOKEN="$BRIDGE_AS_TOKEN" \
BRIDGE_HS_TOKEN="$BRIDGE_HS_TOKEN" \
BRIDGE_PROVISIONING_SECRET="$BRIDGE_PROVISIONING_SECRET" \
BRIDGE_STATUS_ENDPOINT="${BRIDGE_STATUS_ENDPOINT:-}" \
  yq -I4 e -i '
    .homeserver.address = strenv(MATRIX_INTERNAL_URL) |
    .homeserver.domain = strenv(MATRIX_DOMAIN) |
    .homeserver.status_endpoint = strenv(BRIDGE_STATUS_ENDPOINT) |
    .appservice.as_token = strenv(BRIDGE_AS_TOKEN) |
    .appservice.hs_token = strenv(BRIDGE_HS_TOKEN) |
    .provisioning.shared_secret = strenv(BRIDGE_PROVISIONING_SECRET) |
    .bridge.permissions = {strenv(MATRIX_OWNER_USER_ID): "admin"}
  ' "$config"

# What actually builds the registration. It also rewrites the config with the
# upstream defaults of every key the base config left out — which is why the
# token pinning below has to touch the config too.
"$binary" -g -c "$config" -r "$registration"

BRIDGE_AS_TOKEN="$BRIDGE_AS_TOKEN" BRIDGE_HS_TOKEN="$BRIDGE_HS_TOKEN" \
  yq -I4 e -i '
    .appservice.as_token = strenv(BRIDGE_AS_TOKEN) |
    .appservice.hs_token = strenv(BRIDGE_HS_TOKEN)
  ' "$config"

if [ -n "$sender_localpart" ]; then
  SENDER_LOCALPART="$sender_localpart" \
    yq -I4 e -i '.sender_localpart = strenv(SENDER_LOCALPART)' "$registration"
fi
BRIDGE_AS_TOKEN="$BRIDGE_AS_TOKEN" BRIDGE_HS_TOKEN="$BRIDGE_HS_TOKEN" \
  yq -I4 e -i '
    .as_token = strenv(BRIDGE_AS_TOKEN) |
    .hs_token = strenv(BRIDGE_HS_TOKEN)
  ' "$registration"

cp "$registration" "$installed"
# Synapse reads this file as its own user, not the bridge's, so it has to be
# world-readable. It holds both appservice tokens; the named volume it lives
# in is reachable only by root on the host, and protecting that host is the
# operator's (docs/architecture/security-model.md).
chmod 0644 "$installed"

# The bridge itself runs unprivileged (1337 by default in the mautrix images);
# this one-shot runs as root so it can write the shared volume. Note that bash
# owns the name UID, so the image's own UID/GID variables are re-exported
# under BRIDGE_UID/BRIDGE_GID by the compose service.
chown -R "${BRIDGE_UID:-1337}:${BRIDGE_GID:-1337}" "$DATA_DIR"

echo "generate-registration: wrote $installed for appservice '$(yq e '.id' "$installed")' at $(yq e '.url' "$installed")"
