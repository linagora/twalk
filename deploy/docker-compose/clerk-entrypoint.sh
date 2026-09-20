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
# Two more things happen here, because only root can do them: the key is
# handed over, and so is the session. Compose bind-mounts the operator's key
# file — owned by their account, mode 0600, which is how
# provision-nostr-key.sh writes it and the only mode the clerk accepts — and
# the clerk runs as an unprivileged account of this image's own that cannot
# open it. So the file is copied, as root, into the one directory `clerk`
# owns, and the binary is started as `clerk` reading the copy
# (clerk.Dockerfile says why there is no USER line). The session directory
# of the write half (#284) is handed over in place rather than copied,
# because the clerk rewrites it: see below.

set -eu

# The marker the healthcheck accepts in place of /health (below) lives in
# /run/clerk, a tmpfs since the key's copy moved there, so a fresh container
# starts without one; it is still removed here first, because a restart of
# the same container is not a fresh one, and a clerk configured since the
# last start would otherwise count as healthy while hosting nothing — or
# while crashed.
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

# The write half (#284, ADR 0036): the session of the owner's `Buzz` device
# on the Companion Gateway, a refresh token in a file the clerk rewrites on
# every refresh. Compose mounts the host directory named by
# CLERK_GATEWAY_SESSION_DIR at /var/lib/clerk, read-write — a directory and
# never the file, because the clerk writes a temporary file beside it and
# renames it over, which a bind-mounted file cannot be — and /dev/null when
# that variable is unset, which is not a directory. Two things only root
# can do happen here. The directory and the file are handed to the
# unprivileged `clerk` account (the host directory was made by whoever ran
# provision-clerk-device.sh, or by Docker as root), because the rewrite
# creates a file in it and the binary refuses a session file mode looser
# than 0600 (clerk/src/gateway.rs). And a half-configured write half is
# refused with the two things that fix it named, rather than handed to the
# binary as a session file that is not there: an owner's key with no
# session directory, or a directory with no session in it, is an operator
# halfway through `provision-clerk-device.sh`, and a crash loop reading
# "No such file" would not say so.
session_dir=/var/lib/clerk
if [ -d "$session_dir" ]; then
	if [ ! -f "$session_dir/session" ]; then
		echo "clerk refuses to start: CLERK_GATEWAY_SESSION_DIR is mounted at $session_dir but" >&2
		echo "holds no session. Run ./provision-clerk-device.sh — it signs the clerk in to the" >&2
		echo "Companion Gateway as a device of yours named \"Buzz\" and writes the session" >&2
		echo "there — then \`docker compose up -d clerk\`. To run without the write half," >&2
		echo "unset CLERK_GATEWAY_SESSION_DIR and CLERK_OWNER_PUBKEY in .env instead." >&2
		exit 1
	fi
	# The directory is taken over, so it has to be the clerk's alone: an
	# operator who pointed CLERK_GATEWAY_SESSION_DIR at their home or at
	# /etc/twalk must get a refusal, not a home directory owned by uid 999.
	# What may be in it is the session and the temporary files the clerk's
	# own rewrite leaves behind when killed mid-write.
	for entry in "$session_dir"/* "$session_dir"/.[!.]* "$session_dir"/..?*; do
		[ -e "$entry" ] || continue
		case "${entry##*/}" in
		session | .session.*.tmp) ;;
		*)
			echo "clerk refuses to start: $session_dir (CLERK_GATEWAY_SESSION_DIR) holds" >&2
			echo "\`${entry##*/}\`, and the clerk takes that directory over for its own account" >&2
			echo "at start. Name a directory of its own, holding nothing but the session" >&2
			echo "./provision-clerk-device.sh writes, then \`docker compose up -d clerk\`." >&2
			exit 1
			;;
		esac
	done
	chown clerk:clerk "$session_dir" "$session_dir/session"
	chmod 0600 "$session_dir/session"
else
	if [ -n "${CLERK_OWNER_PUBKEY:-}" ]; then
		echo "clerk refuses to start: CLERK_OWNER_PUBKEY is set but CLERK_GATEWAY_SESSION_DIR is" >&2
		echo "not, so there is nowhere for the session the write half needs. Name a host" >&2
		echo "directory in CLERK_GATEWAY_SESSION_DIR, run ./provision-clerk-device.sh to sign" >&2
		echo "the clerk in to the Companion Gateway as a device of yours named \"Buzz\", then" >&2
		echo "\`docker compose up -d clerk\`." >&2
		exit 1
	fi
	# No directory and no owner's key: the read half alone. The binary must
	# see the write half as absent, not half-set — compose leaves the three
	# variables empty, which it reads as unset, and the one that names a
	# path inside this container is unset here outright, so that a value
	# an operator exported by hand cannot point the binary at a file that
	# is not mounted. A CLERK_GATEWAY_URL set on its own is left for the
	# binary to refuse, naming what is missing.
	unset CLERK_GATEWAY_SESSION_FILE
fi

# /run/clerk is a tmpfs (compose.yaml), and Docker mounts a tmpfs with the
# mode and owner of the directory underneath it — the image's `0700 root`,
# not the 1777 a bare `mount -t tmpfs` gives — so the clerk could not
# traverse it to read its own 0600 copy: the first live start failed on
# exactly that. Root hands the directory to the clerk before the copy.
chown clerk:clerk /run/clerk
chmod 0700 /run/clerk
install -o clerk -g clerk -m 0600 "$mount" /run/clerk/key
export CLERK_NOSTR_KEY_FILE=/run/clerk/key
exec setpriv --reuid=clerk --regid=clerk --clear-groups twalk-clerk "$@"
