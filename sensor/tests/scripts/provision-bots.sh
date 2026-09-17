#!/usr/bin/env bash
# Provision the test bot users on the test Synapse. Idempotent: existing
# users are skipped. Passwords follow the scheme test-only-password-<localpart>,
# throwaway constants for the local stack; the Rust harness
# (tests/harness/mod.rs) logs in with the same scheme — keep in sync.
set -euo pipefail

COMPOSE_FILE="$(cd "$(dirname "$0")/.." && pwd)/compose.test.yaml"
COMPOSE="docker compose -p twalk-sensor-test -f $COMPOSE_FILE"

for bot in bot_alpha bot_beta; do
  output=$($COMPOSE exec -T synapse register_new_matrix_user \
      -u "$bot" -p "test-only-password-$bot" --no-admin \
      -c /config/homeserver.yaml http://localhost:8008 2>&1) || true
  if echo "$output" | grep -q "Success"; then
    echo "provisioned $bot"
  elif echo "$output" | grep -qi "already"; then
    echo "$bot already exists, skipped"
  else
    echo "failed to provision $bot:" >&2
    echo "$output" >&2
    exit 1
  fi
done
