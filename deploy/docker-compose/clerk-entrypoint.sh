#!/bin/sh
# The deployment's clerk entrypoint: start the clerk, or say plainly that
# this deployment has not been given a Buzz relay to write to and host
# nothing until it is.
#
# Twalk ships no Buzz relay and no key for one: the clerk refuses to start
# without the URL its relay announces, its own Nostr key and the three
# channels it writes to (clerk/src/config.rs), and it is right to. But this
# service is part of the default stack, and an operator who runs Synapse,
# the bus, the Sensor, the Gateway and Hermes without a relay must still get
# a stack that comes up: a container exiting into a restart loop would make
# `docker compose up -d --wait` fail for a deployment that is working exactly
# as configured.
#
# So the refusal is stated here, once, in the log an operator reads — naming
# the variables that are missing — and the container stays up hosting
# nothing. That is the shape hermes-entrypoint.sh gives the same question,
# and the rest of the platform's for "allowed to exist, handed nothing"
# (ADR 0013).
#
# Anything else — a relay that refuses the key, a channel the clerk is not a
# member of, a bus that is not there — is the clerk's to report, and it is
# already better at it than a shell script would be.
#
# One more thing happens here, because only root can do it: the key is
# handed over. Compose bind-mounts the operator's key file — owned by their
# account, mode 0600, which is how provision-nostr-key.sh writes it and the
# only mode the clerk accepts — and the clerk runs as an unprivileged account
# of this image's own that cannot open it. So the file is copied, as root,
# into the one directory `clerk` owns, and the binary is started as `clerk`
# reading the copy (clerk.Dockerfile says why there is no USER line).

set -eu

# The marker the healthcheck accepts in place of /health (below) is written
# in the container's own filesystem, which a restart keeps: decided afresh on
# every start, or a clerk configured since the last one would still count as
# healthy while hosting nothing — or while crashed.
rm -f /run/clerk/hosting-nothing

required="CLERK_RELAY_URL CLERK_CHANNEL_APPROVALS CLERK_CHANNEL_ACTIVITY CLERK_CHANNEL_JOURNAL"

missing=
for name in $required; do
	eval "value=\${$name:-}"
	[ -n "$value" ] || missing="$missing $name"
done

# Where compose mounted the key. With CLERK_NOSTR_KEY_FILE unset in .env that
# is /dev/null, and an empty file is no key either; neither is a regular file
# with something in it. A directory is a different fact: the host path named
# in .env does not exist and Docker made a directory in its place, which is a
# typo to fix rather than a key to wait for.
mount="${CLERK_NOSTR_KEY_FILE:-/run/secrets/clerk.key}"
if [ -d "$mount" ]; then
	echo "clerk refuses to start: $mount is a directory. The host path CLERK_NOSTR_KEY_FILE" >&2
	echo "names in .env does not exist, so Docker created a directory there instead of" >&2
	echo "mounting a file. Check the path, then \`docker compose up -d clerk\`." >&2
	exit 1
fi
if [ ! -f "$mount" ] || [ ! -s "$mount" ]; then
	missing="$missing CLERK_NOSTR_KEY_FILE"
fi

if [ -n "$missing" ]; then
	echo "clerk is writing to no relay. Unset in this deployment:$missing." >&2
	echo "Twalk ships no Buzz relay and no key for one: name the URL your relay announces," >&2
	echo "the key file ./provision-nostr-key.sh wrote and the three channels" >&2
	echo "./provision-buzz-channels.sh printed, in .env (see" >&2
	echo "deploy/docker-compose/.env.example), then \`docker compose up -d clerk\`. Until" >&2
	echo "then this container runs and hosts nothing, and the rest of the stack is" >&2
	echo "unaffected." >&2
	# What the compose healthcheck accepts in place of /health, which nothing
	# is serving: a container hosting nothing on purpose is not an unhealthy
	# one, and `docker compose up -d --wait` would otherwise fail on it.
	touch /run/clerk/hosting-nothing
	exec setpriv --reuid=clerk --regid=clerk --clear-groups sleep infinity
fi

# The copy below is 0600 whatever the host file is, so the host file's mode
# has to be checked here, where it is still visible: a key readable by group
# or others on the host is readable by every other container that mounts it,
# and the clerk refuses such a file for that reason (clerk/src/relay.rs). The
# same refusal, the same sentence, before the mode is laundered.
mode="$(stat -c %a "$mount")"
case "$mode" in
*00) ;;
*)
	echo "clerk refuses to start: the key file CLERK_NOSTR_KEY_FILE names in .env is" >&2
	echo "readable by group or others (mode $mode); it signs everything the clerk posts, so" >&2
	echo "run \`chmod 0600\` on that host path and \`docker compose up -d clerk\` again." >&2
	exit 1
	;;
esac

install -o clerk -g clerk -m 0600 "$mount" /run/clerk/key
export CLERK_NOSTR_KEY_FILE=/run/clerk/key
exec setpriv --reuid=clerk --regid=clerk --clear-groups twalk-clerk "$@"
