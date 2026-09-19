#!/usr/bin/env bash
# Reset the owner's Matrix password on the reference deployment.
#
# Synapse has no password-reset command: the supported route is the admin API,
# and a deployment provisioned by `provision.sh` has no admin at all (it passes
# `--no-admin` for every account it creates, deliberately). So this creates a
# temporary admin with the registration shared secret the stack already holds,
# resets the owner's password, and deactivates the temporary admin again.
#
#   ./reset-owner-password.sh [output-file]   default: ~/twalk-new-password.txt
#
# **Nothing sensitive is printed.** Both generated secrets stay in shell
# variables; the new password is written to the output file with mode 0600 and
# never echoed, and the temporary admin's password is discarded with the
# process. The only thing on stdout is progress and the path to read.
#
# `logout_devices` is **false** on purpose: a reset that logs out every device
# would sign the owner out of any Matrix client they still have working, which
# is the one thing that might still be holding a usable session.
set -euo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
ENV_FILE="${TWALK_ENV_FILE:-$DEPLOY_DIR/.env}"
OUT="${1:-$HOME/twalk-new-password.txt}"
PROJECT="${TWALK_COMPOSE_PROJECT:-twalk}"
ADMIN_LOCALPART="twalk-recovery-$$"

env_value() { grep -E "^$1=" "$ENV_FILE" | tail -1 | cut -d= -f2-; }
dc() { docker compose -p "$PROJECT" --env-file "$ENV_FILE" -f "$DEPLOY_DIR/compose.yaml" "$@"; }

owner="$(env_value MATRIX_OWNER_USER_ID)"
[ -n "$owner" ] || owner="$(env_value GATEWAY_OWNER)"
port="$(env_value MATRIX_HTTP_PORT)"; port="${port:-8008}"
hs="http://127.0.0.1:$port"
[ -n "$owner" ] || { echo "no owner in $ENV_FILE" >&2; exit 1; }

umask 077
admin_pw="$(openssl rand -base64 24 | tr -d '\n=')"
new_pw="$(openssl rand -base64 18 | tr -d '\n=')"

echo "1/4  creating a temporary admin ($ADMIN_LOCALPART)"
dc exec -T synapse register_new_matrix_user \
  -u "$ADMIN_LOCALPART" -p "$admin_pw" --admin \
  -c /data/homeserver.yaml http://localhost:8008 >/dev/null 2>&1 \
  || { echo "     failed to create the temporary admin" >&2; exit 1; }

echo "2/4  signing it in"
token="$(printf '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"%s"},"password":"%s"}' \
  "$ADMIN_LOCALPART" "$admin_pw" \
  | curl -s -X POST -H 'Content-Type: application/json' --data-binary @- "$hs/_matrix/client/v3/login" \
  | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')"
[ -n "$token" ] || { echo "     the temporary admin could not sign in" >&2; exit 1; }

echo "3/4  resetting the password of $owner"
code="$(printf '{"new_password":"%s","logout_devices":false}' "$new_pw" \
  | curl -s -o /dev/null -w '%{http_code}' -X POST \
      -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
      --data-binary @- "$hs/_synapse/admin/v1/reset_password/$owner")"
if [ "$code" != "200" ]; then
  echo "     the reset was refused (HTTP $code)" >&2
  curl -s -X POST -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
    -d "{\"erase\":false}" "$hs/_synapse/admin/v1/deactivate/@$ADMIN_LOCALPART:$(echo "$owner" | cut -d: -f2)" >/dev/null 2>&1 || true
  exit 1
fi
printf '%s\n' "$new_pw" > "$OUT"
chmod 600 "$OUT"
unset new_pw

echo "4/4  deactivating the temporary admin"
domain="$(echo "$owner" | cut -d: -f2)"
curl -s -o /dev/null -X POST -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  -d '{"erase":false}' "$hs/_synapse/admin/v1/deactivate/@$ADMIN_LOCALPART:$domain" \
  && echo "     deactivated" || echo "     could not deactivate @$ADMIN_LOCALPART:$domain — do it yourself" >&2
unset admin_pw token

echo
echo "Done. The new password for $owner is in:"
echo "    $OUT   (mode 0600, never printed)"
echo
echo "Read it yourself, sign in to the Companion with it, then change it from"
echo "your Matrix client and delete the file."
