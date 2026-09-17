#!/usr/bin/env bash
# Provision the test bot users on the test Synapse. Idempotent: existing
# users are skipped. Passwords follow the scheme test-only-password-<localpart>,
# throwaway constants for the local stack; the Sensor harness's Bot helper
# (sensor/tests/harness/mod.rs) logs in with the same scheme — keep in sync.
set -euo pipefail

# TWALK_TEST_STACK selects the compose project so parallel worktrees each
# run their own isolated stack (defaults to the main checkout's project).
COMPOSE_FILE="$(cd "$(dirname "$0")/.." && pwd)/compose.test.yaml"
COMPOSE="docker compose -p ${TWALK_TEST_STACK:-twalk-sensor-test} -f $COMPOSE_FILE"

# bot_alpha / bot_beta play bridges and contacts; sensor is the account the
# Sensor process logs in as; whatsapp_33612345678 is a ghost-style user for
# the network-prefix tests (mautrix puppet naming convention).
for bot in bot_alpha bot_beta sensor whatsapp_33612345678; do
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
