#!/usr/bin/env bash
# Sign the clerk in to the Companion Gateway as a device of the owner named
# `Buzz`, and write the session where the clerk keeps it alive.
#
#   ./provision-clerk-device.sh [session-dir]   default: CLERK_GATEWAY_SESSION_DIR in ./.env
#
# This is the **operator route** of ADR 0036 (ticket #284). The clerk carries
# a ✅ the owner makes on Buzz to the Companion Gateway as an approval, and
# it does so as what a phone or a browser tab is: **a device of the owner**,
# signed in through `POST /api/session` with a Matrix OpenID token, listed on
# the dashboard as `Buzz`, and revocable there like any other. It holds no
# service token and nothing the Gateway would take for anyone but the
# owner's own device. What it holds is one refresh token, in a file it owns
# and rotates (`POST /api/session/refresh` rotates both tokens; a device
# token lives fifteen minutes, in the clerk's memory only; a refresh token
# thirty days). This script is what creates that file, once, and what
# restores it after the device was revoked or the token died unused.
#
# Compare `provision-owner-device.sh`, which this is modelled on: that one
# creates a **Matrix** device on the owner's account, for the Sensor to post
# replies through; this one creates a **Companion Gateway** device, for the
# clerk to approve through. The Matrix login this script makes is only a
# means to the OpenID token the Gateway signs a device in with, and it is
# logged out at the end — no Matrix credential is written anywhere.
#
# Idempotent, for #227's reason (a credential nobody knows exists is one
# nobody revokes): a session that is still alive is refreshed and left
# alone; a dead one is replaced, and every other unrevoked device named
# `Buzz` is revoked so that there is one.
#
# The password is read from the terminal, or from a file named by
# TWALK_OWNER_PASSWORD_FILE. It is never echoed, never logged, never written
# anywhere, and never sent to anything but the homeserver — not to the
# Companion Gateway, which holds no credential of the owner's Matrix account
# (ADR 0011). No token is ever printed: the Matrix access token travels to
# python on stdin, never on an argv, and the Gateway's tokens go from the
# response straight into the session file (mode 0600, written beside it and
# renamed over).
set -euo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
ENV_FILE="${TWALK_ENV_FILE:-$DEPLOY_DIR/.env}"
DEVICE_NAME="Buzz"

[ -f "$ENV_FILE" ] || { echo "env file not found: $ENV_FILE" >&2; exit 1; }
umask 077

# A variable the file does not set is empty, not a failure: `grep` exits 1 on
# no match and `set -eo pipefail` would otherwise end the script there.
env_value() { { grep -E "^$1=" "$ENV_FILE" || true; } | tail -1 | cut -d= -f2-; }

owner="$(env_value GATEWAY_OWNER)"
[ -n "$owner" ] || owner="$(env_value SENSOR_OWNER)"
[ -n "$owner" ] || owner="$(env_value MATRIX_OWNER_USER_ID)"
if [ -z "$owner" ]; then
  echo "no owner in $ENV_FILE (GATEWAY_OWNER / SENSOR_OWNER): there is nobody to be a device of" >&2
  exit 1
fi
localpart="${owner#@}"; localpart="${localpart%%:*}"

matrix_port="$(env_value MATRIX_HTTP_PORT)"; matrix_port="${matrix_port:-8008}"
hs="http://127.0.0.1:$matrix_port"

# The Gateway as the clerk will reach it: CLERK_GATEWAY_URL when the operator
# set one, else this stack's published port on loopback — the same default
# compose.yaml gives the clerk.
gateway="$(env_value CLERK_GATEWAY_URL)"
if [ -z "$gateway" ]; then
  gateway_port="$(env_value GATEWAY_HTTP_PORT)"; gateway_port="${gateway_port:-8080}"
  gateway="http://127.0.0.1:$gateway_port"
fi

session_dir="${1:-$(env_value CLERK_GATEWAY_SESSION_DIR)}"
if [ -z "$session_dir" ]; then
  echo "no session directory: set CLERK_GATEWAY_SESSION_DIR in $ENV_FILE, or name one as" >&2
  echo "the argument. It is a directory rather than a file because the clerk rewrites the" >&2
  echo "session atomically — a temporary file beside it, renamed over — which a file" >&2
  echo "bind-mounted on its own cannot be." >&2
  exit 1
fi
session="$session_dir/session"

# The directory is created 0700 (umask above). Once the clerk has started
# with it mounted, the container owns it under its own unprivileged account,
# so an operator who is not root comes back to it with sudo — and the file
# written then keeps that account as its owner (write_session below), so the
# running clerk goes on reading it.
mkdir -p "$session_dir"
if [ ! -w "$session_dir" ]; then
  echo "cannot write into $session_dir." >&2
  echo "The clerk's container takes that directory over for its own account the first" >&2
  echo "time it starts with it mounted, so run this with sudo (with" >&2
  echo "--preserve-env=TWALK_OWNER_PASSWORD_FILE if you use that); the file it writes" >&2
  echo "keeps the container's account as its owner." >&2
  exit 1
fi

# The program goes in a file, the secrets on stdin. Passing a token or a
# password as an argument would put it in `ps` for every user on the host,
# which is the kind of leak this script exists not to make. The tokens the
# Gateway issues never come back out to the shell at all: python writes the
# session file itself and prints only the device's id.
prog="$(mktemp)"
trap 'rm -f "$prog"' EXIT
cat > "$prog" <<'PY'
import json, os, sys, urllib.error, urllib.request

SESSION_KEY = "TWALK_GATEWAY_REFRESH_TOKEN"
DEVICE_COOKIE = "twalk_device"
REFRESH_COOKIE = "twalk_refresh"
DEAD = 3  # exit status: the Gateway will not have this session


def call(url, method="GET", body=None, headers=None):
    """One request; (status, headers, parsed body). A 4xx/5xx is an answer,
    not an exception, so the caller can read the Gateway's own error code."""
    data = None if body is None else json.dumps(body).encode()
    h = {"Content-Type": "application/json"} if body is not None else {}
    h.update(headers or {})
    req = urllib.request.Request(url, data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.headers, parse(r.read())
    except urllib.error.HTTPError as e:
        return e.code, e.headers, parse(e.read())
    except urllib.error.URLError as e:
        sys.exit("cannot reach %s: %s" % (url, e.reason))


def parse(raw):
    try:
        return json.loads(raw or b"{}")
    except ValueError:
        return {}


def cookies(headers):
    """The `Set-Cookie` headers as {name: value}; attributes dropped."""
    found = {}
    for line in headers.get_all("Set-Cookie") or []:
        pair = line.split(";", 1)[0]
        if "=" in pair:
            name, value = pair.split("=", 1)
            found[name.strip()] = value.strip()
    return found


def read_session(path):
    try:
        with open(path) as f:
            lines = f.read().splitlines()
    except OSError as e:
        sys.exit("cannot read %s: %s" % (path, e))
    for line in lines:
        line = line.strip()
        if line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):].lstrip()
        if line.startswith(SESSION_KEY):
            rest = line[len(SESSION_KEY):].lstrip()
            if rest.startswith("="):
                return rest[1:].strip().strip('"').strip("'")
    return None


def write_session(path, token):
    """`TWALK_GATEWAY_REFRESH_TOKEN=<token>`, 0600 before a byte is written,
    beside the file and renamed over it — the shape the clerk itself uses,
    so a script killed mid-write leaves the old file or the new one.

    Run as root (the documented `sudo` once the container owns the
    directory), the new file would be root's, and the clerk — an
    unprivileged account that reads this file on every refresh — could
    then open neither the token nor a ✅ until its next container start. So
    the temporary file takes the ownership of the file it replaces, or of
    the directory when there is none yet, and a running clerk picks the
    new token up by itself."""
    tmp = os.path.join(os.path.dirname(path) or ".", ".session.%d.tmp" % os.getpid())
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "w") as f:
            f.write("# The clerk's session on the Companion Gateway: the refresh token of the\n")
            f.write("# owner's device named Buzz (ADR 0036). Written by provision-clerk-device.sh,\n")
            f.write("# rotated by the clerk. Revoke it from the Companion's dashboard.\n")
            f.write("%s=%s\n" % (SESSION_KEY, token))
            f.flush()
            os.fsync(f.fileno())
        if os.geteuid() == 0:
            owner = os.stat(path if os.path.exists(path) else os.path.dirname(path) or ".")
            os.chown(tmp, owner.st_uid, owner.st_gid)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def session_answer(status, headers, body, path):
    """The Gateway's IssuedSession: both tokens are in Set-Cookie, never in
    the body. Writes the refresh token; returns the device cookie and id."""
    jar = cookies(headers)
    refresh, device = jar.get(REFRESH_COOKIE), jar.get(DEVICE_COOKIE)
    if not refresh or not device:
        sys.exit("the Companion Gateway answered %d without both session cookies" % status)
    write_session(path, refresh)
    return device, (body.get("device") or {}).get("id", "?")


def revoke_others(gateway, device, name):
    """One device named `name`: every other one still unrevoked is a
    previous clerk session, dead or forgotten, and goes now. Returns how
    many were revoked here; a 404 is one already gone and is not counted."""
    cookie = {"Cookie": "%s=%s" % (DEVICE_COOKIE, device)}
    status, _h, listed = call(gateway + "/api/devices", headers=cookie)
    if status != 200:
        sys.exit("the Companion Gateway refused the device list: %d %s"
                 % (status, listed.get("error", "")))
    revoked = 0
    for other in listed.get("devices", []):
        if (other.get("name") == name and not other.get("current")
                and other.get("revoked_unix_seconds") is None):
            status, _h, err = call(
                gateway + "/api/devices/" + other["id"], "DELETE", headers=cookie)
            if status == 204:
                revoked += 1
            elif status != 404:
                sys.exit("could not revoke the previous %s device %s: %d %s"
                         % (name, other["id"], status, err.get("error", "")))
    return revoked


def refresh(gateway, path, name):
    token = read_session(path)
    if not token:
        sys.stderr.write("the session file holds no %s line\n" % SESSION_KEY)
        sys.exit(DEAD)
    status, headers, body = call(
        gateway + "/api/session/refresh", "POST",
        headers={"Cookie": "%s=%s" % (REFRESH_COOKIE, token)},
    )
    if status == 401:
        sys.exit(DEAD)
    if status != 200:
        sys.exit("the Companion Gateway refused the refresh: %d %s"
                 % (status, body.get("error", "")))
    device, device_id = session_answer(status, headers, body, path)
    # The refreshed session holds a device cookie too, so "one Buzz" is
    # kept on this path as well: a previous run that wrote the file and
    # then failed a revocation must not leave the extra device standing.
    print(device_id)
    print(revoke_others(gateway, device, name))


def signin(hs, gateway, owner, localpart, name, path):
    password = sys.stdin.read().rstrip("\n")
    status, _h, d = call(hs + "/_matrix/client/v3/login", "POST", {
        "type": "m.login.password",
        "identifier": {"type": "m.id.user", "user": localpart},
        "password": password,
        "initial_device_display_name": "Twalk clerk provisioning",
    })
    del password
    if status != 200:
        sys.exit("the homeserver refused the login: %s %s"
                 % (d.get("errcode", status), d.get("error", "")))
    matrix = {"Authorization": "Bearer " + d["access_token"]}
    try:
        status, _h, openid = call(
            hs + "/_matrix/client/v3/user/%s/openid/request_token" % owner,
            "POST", {}, matrix)
        if status != 200:
            sys.exit("the homeserver refused the OpenID token: %s %s"
                     % (openid.get("errcode", status), openid.get("error", "")))
        status, headers, body = call(gateway + "/api/session", "POST", {
            "matrix_openid_token": {
                "access_token": openid["access_token"],
                "matrix_server_name": openid.get("matrix_server_name"),
            },
            "device_name": name,
        })
        if status != 200:
            sys.exit("the Companion Gateway refused the sign-in: %d %s"
                     % (status, body.get("error", "")))
        device, device_id = session_answer(status, headers, body, path)
        print(device_id)
        print(revoke_others(gateway, device, name))
    finally:
        # The Matrix device existed for the OpenID token and for nothing else.
        call(hs + "/_matrix/client/v3/logout", "POST", {}, matrix)


if sys.argv[1] == "refresh":
    refresh(*sys.argv[2:5])
else:
    signin(*sys.argv[2:8])
PY

# An existing session that is still alive is refreshed and kept: the device
# stays the one on the dashboard, and nobody is asked for a password.
if [ -f "$session" ]; then
  set +e
  answer="$(python3 "$prog" refresh "$gateway" "$session" "$DEVICE_NAME")"
  status=$?
  set -e
  case $status in
  0)
    device_id="$(printf '%s\n' "$answer" | sed -n 1p)"
    revoked="$(printf '%s\n' "$answer" | sed -n 2p)"
    echo "the device \"$DEVICE_NAME\" is alive (id $device_id); its session was refreshed in place."
    if [ "${revoked:-0}" != 0 ]; then
      echo "  revoked    $revoked other device(s) named \"$DEVICE_NAME\""
    fi
    echo "Nothing to do: a running clerk reads the rotated token at its next refresh, and"
    echo "no restart is needed."
    exit 0
    ;;
  3)
    echo "the session in $session is dead: the device was revoked, or its refresh token"
    echo "went unused for thirty days. Signing a new \"$DEVICE_NAME\" device in."
    ;;
  *)
    exit 1
    ;;
  esac
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

echo "signing a device named \"$DEVICE_NAME\" in to $gateway as $owner"
answer="$(printf '%s' "$password" | python3 "$prog" signin "$hs" "$gateway" "$owner" "$localpart" "$DEVICE_NAME" "$session")" || exit 1
unset password

device_id="$(printf '%s\n' "$answer" | sed -n 1p)"
revoked="$(printf '%s\n' "$answer" | sed -n 2p)"
[ -n "$device_id" ] || { echo "the Companion Gateway returned no device" >&2; exit 1; }

echo
echo "  device     \"$DEVICE_NAME\" (id $device_id), on $owner"
echo "  session    written to $session (not shown)"
if [ "${revoked:-0}" != 0 ]; then
  echo "  revoked    $revoked previous device(s) named \"$DEVICE_NAME\""
fi
echo
echo "Restart the clerk to pick it up now (a running one would find it by itself at its"
echo "next hourly retry; \`up -d\` does not restart a container whose configuration is"
echo "unchanged):"
echo "    docker compose restart clerk"
echo "or, if it is not running yet:"
echo "    docker compose up -d clerk"
echo
echo "Revoke it any time from the Companion's dashboard, like a phone: the next ✅"
echo "on Buzz is then answered in its thread, and this script restores the device."
