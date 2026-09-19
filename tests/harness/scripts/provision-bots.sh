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
#
# whatsapp_33660469852 and whatsapp_lid-115332874281144 play the *operator's*
# own ghosts (#109, ADR 0018), and there are two of them on one network on
# purpose: that is what the reference deployment answers for one WhatsApp
# account — a phone-number ghost and a LID ghost — and the messages the owner
# sent from their phone arrived under the LID one.
# whatsappbot and signalbot play the *bridges' own bots* (#152, ADR 0026) —
# mautrix's sender_localpart, the appservice's service identity, which is
# neither the owner nor a contact. Two of them because the reference
# deployment ran two bridges and a fix that recognised one would have halved
# the defect rather than closed it.
#
# bot_gamma and bot_delta play native Matrix contacts (#150, ADR 0027): a real
# Matrix user whose localpart carries no network prefix, and who can therefore
# only be attributed to a network by the rooms they are in. whatsapp_33698765432
# is the ghost of the same suite — attributed by its own localpart, whatever
# rooms it is in. All three are separate accounts from bot_beta and from
# whatsapp_33612345678, because the presence suites depend on exactly which
# rooms a subject is joined to and must not have to share one.
# owner plays the deployment's one owner as a **real account with a real
# device** (#123, ADR 0025): the identity Twalk acts through when it posts an
# approved reply, which a mautrix bridge relays because it really is the user.
# Every other suite names an owner the Sensor only ever publishes
# (@michel:test.twalk, an account that does not exist); this one has to log in,
# hand its device token to the Sensor and join portal rooms, so it is its own
# account and is deliberately in none of the presence suites' rooms.
for bot in bot_alpha bot_beta bot_gamma bot_delta sensor owner whatsapp_33612345678 \
           whatsapp_33660469852 whatsapp_lid-115332874281144 \
           whatsapp_33698765432 whatsappbot signalbot; do
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
