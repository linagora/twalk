#!/usr/bin/env sh
# Ask what one of the owner's events carries (skills/twalk-calendar, #355).
#
#   event-facts.sh <uid>                 # the deployment's own calendar
#   event-facts.sh <connection> <uid>    # naming it, when there are two
#
# Signs `GET /_twalk/hermes/event-facts` with TWALK_ANSWER_SECRET — the
# secret the outbound hook signs answers with — over the request line, and
# prints the Gateway's JSON answer. Needs curl and openssl.
#
# What comes back is facts, never the event's words: whether it has a join
# link and which, how long its description is, how many attachments it has.
# The description's text is not readable through this or any other route.
set -eu

: "${TWALK_GATEWAY_URL:?TWALK_GATEWAY_URL is the owner's Companion Gateway origin}"
: "${TWALK_ANSWER_SECRET:?TWALK_ANSWER_SECRET is the secret the outbound hook signs with}"

# One argument means the connection comes from the deployment, for the reason
# `freebusy.sh` states: the agent has no id to name and a guess is refused.
if [ "$#" -eq 1 ]; then
  connection=${TWALK_CALENDAR_CONNECTION:?one argument means the connection comes from TWALK_CALENDAR_CONNECTION, which is unset}
  uid=$1
elif [ "$#" -eq 2 ]; then
  connection=$1
  uid=$2
else
  echo "usage: $0 [<connection>] <uid>   (uid: the event's iCalendar UID, as calendar.event.* carried it; the connection defaults to TWALK_CALENDAR_CONNECTION)" >&2
  exit 64
fi
path=/_twalk/hermes/event-facts
# The query string is signed exactly as sent, so it is built once and used
# twice. A UID is opaque and may carry anything, so it is encoded the same
# way the instants are in `freebusy.sh`.
encode() {
  printf '%s' "$1" | sed -e 's/%/%25/g' -e 's/:/%3A/g' -e 's/+/%2B/g' -e 's/ /%20/g' \
    -e 's/&/%26/g' -e 's/=/%3D/g' -e 's/#/%23/g' -e 's/?/%3F/g' -e 's|/|%2F|g'
}
query="connection=$(encode "$connection")&uid=$(encode "$uid")"
timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
# The secret is an argument to openssl here, visible in the process list for
# the milliseconds the call runs: Hermes's host is the owner's own (ADR
# 0032's allowlist holds one key). On a shared host, sign otherwise.
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
  echo "the Gateway refused the read with HTTP $status:" >&2
  cat "$body" >&2
  echo >&2
  exit 1
fi
