#!/usr/bin/env sh
# Read the owner's free/busy through their Twalk deployment (skills/twalk-calendar).
#
#   freebusy.sh <from> <to>                 # the deployment's own calendar
#   freebusy.sh <connection> <from> <to>    # naming it, when there are two
#
# Signs `GET /_twalk/hermes/freebusy` with TWALK_ANSWER_SECRET — the secret
# the outbound hook signs answers with — over the request line, and prints
# the Gateway's JSON answer. Needs curl and openssl.
#
# The connection may be left out because an agent drafting a reply has no way
# to know one: the message it was woken for carries the *kind* of connection
# it arrived on and never an id (ADR 0033), and the calendar's is a different
# connection anyway. So the deployment names it once, beside the two values
# this script already needs, and the agent asks the question rather than
# guessing an answer the Gateway would refuse (#363).
set -eu

# Two ways to fail, and two codes, because a caller who cannot tell them
# apart cannot act on either: 64 means this call was wrong, 78 means this
# deployment never finished installing the skill. Written out rather than
# `${VAR:?...}` because that form carries its message inside `${...}`, where
# an apostrophe makes the whole file unparseable to bash — and `#!/usr/bin/env
# sh` is bash on any host whose /bin/sh is one.
missing() {
  echo "$0: $1 is unset — $2. This deployment has not finished installing twalk-calendar; nothing was read and nothing reached the Gateway, so this message is the only trace." >&2
  exit 78
}
misused() {
  echo "$0: $1" >&2
  echo "usage: $0 [<connection>] $2" >&2
  exit 64
}

[ -n "${TWALK_GATEWAY_URL:-}" ] || missing TWALK_GATEWAY_URL "the owner's Companion Gateway origin"
[ -n "${TWALK_ANSWER_SECRET:-}" ] || missing TWALK_ANSWER_SECRET "the secret the outbound hook signs answers with"

if [ "$#" -eq 2 ]; then
  [ -n "${TWALK_CALENDAR_CONNECTION:-}" ] \
    || missing TWALK_CALENDAR_CONNECTION "the calendar connection this deployment reads"
  connection=$TWALK_CALENDAR_CONNECTION
  from=$1
  to=$2
elif [ "$#" -eq 3 ]; then
  connection=$1
  from=$2
  to=$3
else
  misused "$# arguments" "<from> <to>   (RFC 3339, at most 14 days apart; the connection defaults to TWALK_CALENDAR_CONNECTION)"
fi

# A three-argument call with its connection dropped is a two-argument call
# with a connection name where an instant should be. Caught here: the Gateway
# would refuse it as `invalid_window`, which sends the reader to look at
# their clock instead of at their command line.
case $from in
  [0-9][0-9][0-9][0-9]-*) ;;
  *) misused "\"$from\" is not an RFC 3339 instant (did you mean it as the connection, with the two dates after it?)" \
       "<from> <to>   (RFC 3339, at most 14 days apart)" ;;
esac
path=/_twalk/hermes/freebusy
# The query string is signed exactly as sent, so it is built once and used
# twice: RFC 3339 instants carry ':' and '+', which are percent-encoded here
# so the URL and the signed line agree byte for byte.
encode() {
  printf '%s' "$1" | sed -e 's/%/%25/g' -e 's/:/%3A/g' -e 's/+/%2B/g' -e 's/ /%20/g' \
    -e 's/&/%26/g' -e 's/=/%3D/g' -e 's/#/%23/g'
}
query="connection=$(encode "$connection")&from=$(encode "$from")&to=$(encode "$to")"
timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
# The secret is an argument to openssl here, visible in the process list
# for the milliseconds the call runs: Hermes's host is the owner's own
# (ADR 0032's allowlist holds one key). On a shared host, sign otherwise.
signature=$(printf 'GET\n%s\n%s\n%s' "$path" "$query" "$timestamp" \
  | openssl dgst -sha256 -hmac "$TWALK_ANSWER_SECRET" | sed 's/^.* //')
delivery=${TWALK_DELIVERY_ID:-$(date -u +%s)-$$}

body=$(mktemp)
trap 'rm -f "$body"' EXIT
status=$(curl -sS -o "$body" -w '%{http_code}' \
  -H "X-Hermes-Timestamp: $timestamp" \
  -H "X-Hermes-Signature-256: sha256=$signature" \
  -H "X-Hermes-Delivery: $delivery" \
  "${TWALK_GATEWAY_URL%/}$path?$query")
if [ "$status" = "200" ]; then
  cat "$body"
  echo
else
  echo "the Gateway refused the free/busy read with HTTP $status:" >&2
  cat "$body" >&2
  echo >&2
  exit 1
fi
