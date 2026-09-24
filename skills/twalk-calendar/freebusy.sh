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

: "${TWALK_GATEWAY_URL:?TWALK_GATEWAY_URL is the owner's Companion Gateway origin}"
: "${TWALK_ANSWER_SECRET:?TWALK_ANSWER_SECRET is the secret the outbound hook signs with}"

if [ "$#" -eq 2 ]; then
  connection=${TWALK_CALENDAR_CONNECTION:?two arguments means the connection comes from TWALK_CALENDAR_CONNECTION, which is unset}
  from=$1
  to=$2
elif [ "$#" -eq 3 ]; then
  connection=$1
  from=$2
  to=$3
else
  echo "usage: $0 [<connection>] <from> <to>   (from/to: RFC 3339, at most 14 days apart; the connection defaults to TWALK_CALENDAR_CONNECTION)" >&2
  exit 64
fi
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
