#!/usr/bin/env bash
# Create the owner's four Buzz channels, as the owner, and tell Hermes and the
# clerk which they are.
#
#   ./provision-buzz-channels.sh [--bot <64-hex pubkey>]... [hermes-env-file]
#                                default env file: ~/.hermes-twalk/.env
#
# This is the **operator route** of issue #218, the way `provision-owner-device.sh`
# is the operator route of ADR 0034: the ticket puts this in the Companion's
# onboarding, with the UUIDs recorded in the Gateway's settings store; until
# that exists, this script is the one way to reach the same state — four
# channels, the owner a member of each, Hermes and the clerk their `bot`
# members, their ids where each reads them, and **no UUID typed by hand**.
#
# Who signs. The channels are the owner's, so they are created with the
# **owner's** key, which this script never receives as an argument and never
# prints: it is read from a file the owner writes themselves, mode 0600,
# named by TWALK_BUZZ_OWNER_KEY_FILE (default ~/.config/twalk/buzz-owner.key,
# one line, nsec or hex). Hermes's key is not used here and would be the wrong
# author: a channel created by the agent is the agent's, and ADR 0032 makes
# Buzz a surface the owner holds. Hermes's *public* key is read from its env
# file, where `provision-nostr-key.sh` wrote it — and when that file holds no
# key, Hermes is simply not added, with a note: a deployment may run the clerk
# and no Hermes, and the channels are the owner's either way.
#
# `--bot <pubkey>`, repeatable, adds another writer in the same `bot` role: the
# clerk (#265), whose key `provision-nostr-key.sh` wrote into a file of its own.
# It is parsed before the env file, so `--bot <hex> /etc/hermes/.env` and
# `--bot <hex>` alone both mean what they read as. The value is the 64-character
# hex public key and nothing else — an npub is refused rather than decoded,
# because the relay is told the hex form (`./run.sh add-member`) and one
# spelling in two places is one fewer thing to get wrong.
#
# Four channels, in the owner's interface language (TWALK_USER_LANGUAGE, `fr`
# or `en`, default `fr`), all private because they carry other people's
# words. The UUID is the identity and the name is only display: a re-run finds
# each channel by its exact name and creates nothing twice, and changing the
# language later does not rename them — a rename would break every stored
# reference for cosmetics.
#
# What is written into Hermes's env file: BUZZ_CHANNELS (the four UUIDs, the
# ones its adapter watches) and BUZZ_HOME_CHANNEL (the `hermes` channel, where
# the owner talks to the agent). Hermes reads them at its next start. What is
# only **printed**: the three CLERK_CHANNEL_* lines for deploy/docker-compose/.env,
# because that file is the compose stack's and this script writes Hermes's.
set -euo pipefail

usage() {
	cat <<'EOF'
usage: ./provision-buzz-channels.sh [--bot <64-hex pubkey>]... [hermes-env-file]
       default env file: ~/.hermes-twalk/.env; --bot is repeatable and adds
       that public key to every channel in the `bot` role (the clerk, #265)
EOF
}

BOTS=()
while [ $# -gt 0 ]; do
	case "$1" in
	--help | -h)
		usage
		exit 0
		;;
	--bot)
		[ $# -ge 2 ] || { echo "--bot takes the 64-character hex public key to add" >&2; usage >&2; exit 2; }
		if ! printf '%s' "$2" | grep -qE '^[0-9a-fA-F]{64}$'; then
			echo "--bot wants a 64-character hex public key, the form the relay was told with add-member; got '$2'" >&2
			exit 2
		fi
		BOTS+=("$(printf '%s' "$2" | tr 'A-F' 'a-f')")
		shift 2
		;;
	--bot=*)
		set -- --bot "${1#--bot=}" "${@:2}"
		;;
	--)
		shift
		break
		;;
	-*)
		echo "unknown option: $1" >&2
		usage >&2
		exit 2
		;;
	*)
		break
		;;
	esac
done
[ $# -le 1 ] || { echo "one env file at most; got: $*" >&2; usage >&2; exit 2; }

ENV_FILE="${1:-${HERMES_HOME:-$HOME/.hermes-twalk}/.env}"
KEY_FILE="${TWALK_BUZZ_OWNER_KEY_FILE:-$HOME/.config/twalk/buzz-owner.key}"
LANGUAGE="${TWALK_USER_LANGUAGE:-fr}"
export BUZZ_RELAY_URL="${BUZZ_RELAY_URL:-http://127.0.0.1:3000}"

command -v buzz >/dev/null || { echo "the buzz CLI is not on PATH" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 is needed to read the CLI's answers" >&2; exit 1; }

# Hermes is added only when its env file holds a key. No file, or a file with
# no key, is a deployment without Hermes yet — the channels are still the
# owner's and the clerk can still be added — so it is a note and not a refusal.
HERMES_PUBKEY=""
if [ -f "$ENV_FILE" ]; then
	HERMES_PUBKEY="$(sed -nE 's/^BUZZ_PUBLIC_KEY=//p' "$ENV_FILE" | head -1 | tr -d "\"'")"
	if [ -z "$HERMES_PUBKEY" ] && grep -qE '^BUZZ_PRIVATE_KEY=' "$ENV_FILE"; then
		echo "$ENV_FILE holds a BUZZ_PRIVATE_KEY and no BUZZ_PUBLIC_KEY: run provision-nostr-key.sh $ENV_FILE" >&2
		echo "once, which writes the public half beside it, and then this again" >&2
		exit 1
	fi
fi
if [ -n "$HERMES_PUBKEY" ]; then
	printf '%s' "$HERMES_PUBKEY" | grep -qE '^[0-9a-f]{64}$' || { echo "BUZZ_PUBLIC_KEY in $ENV_FILE is not a 64-character hex key: '$HERMES_PUBKEY'" >&2; exit 1; }
else
	echo "note: no BUZZ_PUBLIC_KEY in $ENV_FILE, so Hermes is not added to the channels;"
	echo "      run provision-nostr-key.sh $ENV_FILE and this script again when it exists."
fi
[ ${#BOTS[@]} -gt 0 ] || [ -n "$HERMES_PUBKEY" ] || echo "note: no --bot either, so the channels will have the owner as their only member."

if [ ! -f "$KEY_FILE" ]; then
	cat >&2 <<EOF
The owner's Buzz key is not at $KEY_FILE.

Write it there yourself — one line, the nsec or the 64-character hex private
key that Buzz Desktop shows for your identity — and make the file yours alone:

  mkdir -p "$(dirname "$KEY_FILE")"
  umask 077 && \$EDITOR "$KEY_FILE"
  chmod 0600 "$KEY_FILE"

Nothing in Twalk reads that file but this script, and this script never
prints it or passes it on a command line.
EOF
	exit 1
fi
mode="$(stat -c %a "$KEY_FILE")"
[ "$mode" = "600" ] || [ "$mode" = "400" ] || {
	echo "$KEY_FILE is mode $mode; it holds your identity, make it 0600" >&2
	exit 1
}
BUZZ_PRIVATE_KEY="$(head -1 "$KEY_FILE" | tr -d '[:space:]')"
export BUZZ_PRIVATE_KEY

case "$LANGUAGE" in
fr)
	NAMES=(approbations activite hermes journal)
	DESCRIPTIONS=(
		"Ce qui attend un oui ou un non de vous. Un fil par proposition."
		"Ce que votre assistant a remarqué ou fait sans avoir besoin de vous."
		"Vous et Hermes : vos questions, ses réponses."
		"Ce qui est réellement parti sous votre identité, à qui, et si c'est arrivé."
	)
	;;
en)
	NAMES=(approvals activity hermes journal)
	DESCRIPTIONS=(
		"What is waiting for a yes or no from you. One thread per proposal."
		"What your assistant noticed or did without needing you."
		"You and Hermes: your questions, its answers."
		"What actually went out under your identity, to whom, and whether it arrived."
	)
	;;
*)
	echo "TWALK_USER_LANGUAGE must be fr or en (got '$LANGUAGE')" >&2
	exit 1
	;;
esac
TYPES=(forum stream stream stream)

# One field out of one JSON answer, or nothing. The CLI's errors are JSON on
# stderr with an `error` category; a refused write is answered by name below.
field() {
	python3 -c 'import json,sys
value=json.load(sys.stdin)
for key in sys.argv[1:]:
    try:
        value=value[int(key)] if isinstance(value,list) else value.get(key)
    except (IndexError, ValueError, AttributeError):
        sys.exit(0)
    if value is None: sys.exit(0)
print(value)' "$@"
}

refused() {
	# The relay refusing a write from a key it does not know is its own
	# sentence (#218): the fix is `./run.sh add-member`, not a retry.
	if printf '%s' "$1" | grep -qiE 'restricted|not a member|membership|unauthori[sz]ed|forbidden|auth'; then
		echo "the relay refused a write from the owner's key: is $KEY_FILE the identity the relay" >&2
		echo "was told is its owner (RELAY_OWNER_PUBKEY)? The relay answered: $1" >&2
		exit 1
	fi
}

add_bot() {
	local channel="$1" name="$2" pubkey="$3" who="$4" membership
	membership="$(buzz channels add-member --channel "$channel" --pubkey "$pubkey" --role bot 2>&1)" || true
	if printf '%s' "$membership" | grep -q '"accepted":true'; then
		echo "  $who is a bot member"
	elif printf '%s' "$membership" | grep -qi 'missing p tag'; then
		echo "  the key in $KEY_FILE is $who's own, so these channels would be $who's and not yours:" >&2
		echo "  put YOUR identity there (nsec or hex from Buzz Desktop) and run this again" >&2
		exit 1
	else
		refused "$membership"
		echo "  could not add $who to $name: $membership" >&2
		exit 1
	fi
}

UUIDS=()
for i in 0 1 2 3; do
	name="${NAMES[$i]}"
	existing="$(buzz channels search --query "$name" --exact 2>/dev/null | field 0 channel_id || true)"
	if [ -n "$existing" ]; then
		echo "$name: exists, $existing"
		UUIDS+=("$existing")
	else
		answer="$(buzz channels create --name "$name" --type "${TYPES[$i]}" --visibility private --description "${DESCRIPTIONS[$i]}" 2>&1)" || { refused "$answer"; echo "$answer" >&2; exit 1; }
		created="$(printf '%s' "$answer" | field channel_id)"
		[ -n "$created" ] || { refused "$answer"; echo "could not create $name: $answer" >&2; exit 1; }
		echo "$name: created, $created"
		UUIDS+=("$created")
	fi
	# Hermes and the clerk read and write here as themselves, in Buzz's own
	# `bot` role. Adding a member who is already one is accepted again by the
	# relay (it restates the role), so a re-run is a no-op here too. The one
	# refusal that is not one: the relay answers "missing p tag" to an account
	# adding *itself*, which is what happens when the key in $KEY_FILE is
	# Hermes's or the clerk's own — the channels would then be theirs, not
	# the owner's.
	if [ -n "$HERMES_PUBKEY" ]; then
		add_bot "${UUIDS[$i]}" "$name" "$HERMES_PUBKEY" "Hermes"
	fi
	for bot in ${BOTS[@]+"${BOTS[@]}"}; do
		add_bot "${UUIDS[$i]}" "$name" "$bot" "${bot:0:12}…"
	done
done

joined="$(IFS=,; echo "${UUIDS[*]}")"
home="${UUIDS[2]}"
umask 077
[ -f "$ENV_FILE" ] || { mkdir -p "$(dirname "$ENV_FILE")"; touch "$ENV_FILE"; chmod 0600 "$ENV_FILE"; echo "created $ENV_FILE (mode 0600)"; }
for line in "BUZZ_CHANNELS=$joined" "BUZZ_HOME_CHANNEL=$home" "BUZZ_RELAY_URL=$BUZZ_RELAY_URL"; do
	key="${line%%=*}"
	if grep -qE "^$key=" "$ENV_FILE"; then
		sed -i -E "s|^$key=.*|$line|" "$ENV_FILE"
	else
		printf '%s\n' "$line" >> "$ENV_FILE"
	fi
done

cat <<EOF

Written into $ENV_FILE:
  BUZZ_CHANNELS=$joined
  BUZZ_HOME_CHANNEL=$home   (${NAMES[2]})
  BUZZ_RELAY_URL=$BUZZ_RELAY_URL
Hermes reads them at its next start. The owner's key was read from $KEY_FILE
and went nowhere else.

For deploy/docker-compose/.env — the clerk's three channels, which this script
does not write there because that file is the compose stack's:
  CLERK_CHANNEL_APPROVALS=${UUIDS[0]}
  CLERK_CHANNEL_ACTIVITY=${UUIDS[1]}
  CLERK_CHANNEL_JOURNAL=${UUIDS[3]}
EOF
