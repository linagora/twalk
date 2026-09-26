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

# One argument means the connection comes from the deployment, for the reason
# `freebusy.sh` states: the agent has no id to name and a guess is refused.
if [ "$#" -eq 1 ]; then
  [ -n "${TWALK_CALENDAR_CONNECTION:-}" ] \
    || missing TWALK_CALENDAR_CONNECTION "the calendar connection this deployment reads"
  connection=$TWALK_CALENDAR_CONNECTION
  uid=$1
elif [ "$#" -eq 2 ]; then
  connection=$1
  uid=$2
else
  misused "$# arguments" "<uid>   (the event's iCalendar UID, as calendar.event.* carried it in data.uid)"
fi

# A two-argument call with its uid dropped leaves the connection standing
# where the uid should be, and the Gateway answers `found: false` — which
# reads as "the owner has no such event" and is a different and wrong story.
if [ "$uid" = "${TWALK_CALENDAR_CONNECTION:-}" ]; then
  misused "\"$uid\" is this deployment's calendar connection, not an event uid" \
    "<uid>   (the event's iCalendar UID, as calendar.event.* carried it in data.uid)"
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
