#!/usr/bin/env sh
# Read the owner's free/busy through their Twalk deployment (skills/twalk-calendar).
#
#   freebusy.sh <connection> <from> <to>
#
# Signs `GET /_twalk/hermes/freebusy` with TWALK_ANSWER_SECRET — the secret
# the outbound hook signs answers with — over the request line, and prints
# the Gateway's JSON answer. Needs curl and openssl.
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <connection> <from> <to>   (from/to: RFC 3339, at most 14 days apart)" >&2
  exit 64
fi
: "${TWALK_GATEWAY_URL:?TWALK_GATEWAY_URL is the owner's Companion Gateway origin}"
: "${TWALK_ANSWER_SECRET:?TWALK_ANSWER_SECRET is the secret the outbound hook signs with}"

connection=$1
from=$2
to=$3
path=/_twalk/hermes/freebusy
# The query string is signed exactly as sent, so it is built once and used
# twice: RFC 3339 instants carry ':' and '+', which are percent-encoded here
# so the URL and the signed line agree byte for byte.
encode() {
  printf '%s' "$1" | sed -e 's/%/%25/g' -e 's/:/%3A/g' -e 's/+/%2B/g' -e 's/ /%20/g'
}
query="connection=$(encode "$connection")&from=$(encode "$from")&to=$(encode "$to")"
timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
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
