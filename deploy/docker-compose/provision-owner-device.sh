#!/usr/bin/env bash
# Create the device Twalk acts as the owner through, on the owner's own
# account, and write its credential where the Sensor reads it.
#
#   ./provision-owner-device.sh [env-file]      default: ./.env
#
# This is the **operator route** of ADR 0034. That ADR chose the Companion as
# the place this normally happens — the browser creates the device and hands
# its credential to the Sensor over an Olm-encrypted to-device message, so the
# credential never reaches the Gateway at all — and kept this route as the
# documented fallback: for a deployment whose Companion cannot reach the
# Sensor's device, and for proving the acting identity before the browser half
# exists.
#
# Why a device at all: a bridge relays to its network only what the logged-in
# user's own Matrix account sends. A message from `@sensor:` is not a message
# from the user, so the bridge ignores it — without a log line. That is
# issue #123, and it is why every approval made before this existed was a
# reply that never arrived.
#
# What this device is, and is not. It is **write-only**: it joins portal rooms
# and posts approved replies, and it reads no history — so it needs no
# cross-signing, no key backup and no recovery key, and this script asks for
# none. It appears in the owner's device list under a name they will recognise
# and is revocable from any Matrix client without Twalk's involvement, which is
# the mitigation ADR 0025 accepted in exchange for a long-lived credential at
# rest.
#
# The password is read from the terminal, or from a file named by
# TWALK_OWNER_PASSWORD_FILE. It is never echoed, never logged, never written
# anywhere, and never sent to anything but the homeserver — in particular not
# to the Companion Gateway, which holds no Matrix access token (ADR 0011) and
# does not hold this one either.
set -euo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
ENV_FILE="${1:-${TWALK_ENV_FILE:-$DEPLOY_DIR/.env}}"
DISPLAY_NAME="${TWALK_OWNER_DEVICE_NAME:-Twalk}"

[ -f "$ENV_FILE" ] || { echo "env file not found: $ENV_FILE" >&2; exit 1; }
umask 077

env_value() { grep -E "^$1=" "$ENV_FILE" | tail -1 | cut -d= -f2-; }

owner="$(env_value SENSOR_OWNER)"
[ -n "$owner" ] || owner="$(env_value GATEWAY_OWNER)"
[ -n "$owner" ] || owner="$(env_value MATRIX_OWNER_USER_ID)"
port="$(env_value MATRIX_HTTP_PORT)"; port="${port:-8008}"
hs="http://127.0.0.1:$port"

if [ -z "$owner" ]; then
  echo "no owner in $ENV_FILE (SENSOR_OWNER / GATEWAY_OWNER): there is nothing to act as" >&2
  exit 1
fi
localpart="${owner#@}"; localpart="${localpart%%:*}"

# Refuse to create a second device by accident: one nobody knows exists is a
# credential nobody will ever revoke.
if [ -n "$(env_value SENSOR_OWNER_DEVICE_ACCESS_TOKEN)" ]; then
  echo "$ENV_FILE already carries SENSOR_OWNER_DEVICE_ACCESS_TOKEN." >&2
  echo "Remove that line yourself first if you mean to replace the device, and" >&2
  echo "revoke the old one from any Matrix client — it stays valid until you do." >&2
  exit 1
fi

if [ -n "${TWALK_OWNER_PASSWORD_FILE:-}" ]; then
  [ -r "$TWALK_OWNER_PASSWORD_FILE" ] || { echo "cannot read $TWALK_OWNER_PASSWORD_FILE" >&2; exit 1; }
  password="$(cat "$TWALK_OWNER_PASSWORD_FILE")"
else
  printf 'Matrix password for %s: ' "$owner" >&2
  read -rs password
  printf '\n' >&2
fi
[ -n "$password" ] || { echo "no password given" >&2; exit 1; }

echo "creating a device named \"$DISPLAY_NAME\" on $owner"
# The program goes in a file and the password on stdin: one redirection each.
# Passing the password as an argument would put it in `ps` for every user on
# the host, which is the kind of leak this whole script exists not to make.
prog="$(mktemp)"
trap 'rm -f "$prog"' EXIT
cat > "$prog" <<'PY'
import json, sys, urllib.error, urllib.request
hs, localpart, name = sys.argv[1], sys.argv[2], sys.argv[3]
password = sys.stdin.read().rstrip("\n")
body = json.dumps({
    "type": "m.login.password",
    "identifier": {"type": "m.id.user", "user": localpart},
    "password": password,
    "initial_device_display_name": name,
}).encode()
req = urllib.request.Request(hs + "/_matrix/client/v3/login", data=body,
                             headers={"Content-Type": "application/json"})
try:
    with urllib.request.urlopen(req) as r:
        d = json.loads(r.read())
except urllib.error.HTTPError as e:
    d = json.loads(e.read() or b"{}")
    sys.exit("the homeserver refused the login: %s %s"
             % (d.get("errcode", e.code), d.get("error", "")))
print("%s\n%s" % (d["device_id"], d["access_token"]))
PY
answer="$(printf '%s' "$password" | python3 "$prog" "$hs" "$localpart" "$DISPLAY_NAME")" || exit 1
unset password

device_id="$(printf '%s\n' "$answer" | sed -n 1p)"
token="$(printf '%s\n' "$answer" | sed -n 2p)"
unset answer
[ -n "$device_id" ] && [ -n "$token" ] || { echo "the homeserver returned no device" >&2; exit 1; }

cp -p "$ENV_FILE" "$ENV_FILE.bak-before-owner-device-$(date +%s)"
{
  printf '\n# The device Twalk acts as the owner through (ADR 0025, ADR 0034, #123).\n'
  printf '# Created by provision-owner-device.sh. Revoke it from any Matrix client:\n'
  printf '#   Settings > Sessions > "%s" > Sign out.\n' "$DISPLAY_NAME"
  printf 'SENSOR_OWNER_DEVICE_ID=%s\n' "$device_id"
  printf 'SENSOR_OWNER_DEVICE_ACCESS_TOKEN=%s\n' "$token"
} >> "$ENV_FILE"
unset token

echo
echo "  device_id  $device_id"
echo "  token      written to $ENV_FILE (not shown)"
echo
echo "Restart the Sensor to pick it up:"
echo "    docker compose up -d sensor"
echo
echo "Revoke it any time, from any Matrix client, without Twalk:"
echo "    Settings > Sessions > \"$DISPLAY_NAME\" > Sign out"
