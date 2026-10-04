#!/bin/sh
# The collector's entrypoint (#274): hands the binary the client secret and
# the grant directory as its own account, then runs it — or, with
# `authorize` as the first argument, runs the authorize command that gives the
# collector its grant (`docker compose run --rm collector authorize`).
#
# The client secret is a bind mount of a host file the operator's own
# account owns, mode 0600 — the collector refuses anything looser, since
# the secret is what makes it this SSO's client — and no fixed uid this
# image could pick owns a file on a host it has never seen. So the
# entrypoint, as root, copies the secret into /run/collector for the
# `collector` account, and everything after that line runs as that account.
set -eu

# A collector whose credential is a username and a password (#342) has no
# client secret and no grant: one file, checked by the same rule, and the
# OIDC block below is skipped entirely. Two processes, two secrets — the
# calendar collector's password never sees the mailbox's grant.
if [ "${COLLECTOR_CREDENTIAL:-oidc}" = "basic" ]; then
	mount=/run/secrets/collector-basic-password
	if [ -d "$mount" ]; then
		echo "collector refuses to start: $mount is a directory. The host path" >&2
		echo "COLLECTOR_BASIC_PASSWORD_FILE names in .env does not exist, so Docker created a" >&2
		echo "directory there instead of mounting a file." >&2
		exit 1
	fi
	if [ ! -f "$mount" ] || [ ! -s "$mount" ]; then
		echo "collector refuses to start: no password at $mount. Put the service account's" >&2
		echo "password in a file of your own, mode 0600, and name it in .env as" >&2
		echo "COLLECTOR_BASIC_PASSWORD_FILE (see deploy/docker-compose/.env.example)." >&2
		exit 1
	fi
	mode="$(stat -c %a "$mount")"
	case "$mode" in
	*00) ;;
	*)
		echo "collector refuses to start: the password file is readable by group or others" >&2
		echo "(mode $mode). Run \`chmod 0600\` on that host path and start again." >&2
		exit 1
		;;
	esac
	install -m 0600 -o collector -g collector "$mount" /run/collector/basic-password
	chown collector:collector /data
	export COLLECTOR_BASIC_PASSWORD_FILE=/run/collector/basic-password
	exec setpriv --reuid=collector --regid=collector --clear-groups /usr/local/bin/twalk-collector "$@"
fi

# Where compose.yaml mounts the host file; one place, on both sides.
mount=/run/secrets/collector-client-secret
if [ -d "$mount" ]; then
	echo "collector refuses to start: $mount is a directory. The host path" >&2
	echo "COLLECTOR_OIDC_CLIENT_SECRET_FILE names in .env does not exist, so Docker created a" >&2
	echo "directory there instead of mounting a file. Check the path, then" >&2
	echo "\`docker compose --profile collector up -d collector\`." >&2
	exit 1
fi
if [ ! -f "$mount" ] || [ ! -s "$mount" ]; then
	echo "collector refuses to start: no client secret at $mount. Put the OIDC client's" >&2
	echo "secret in a file of your own, mode 0600, and name it in .env as" >&2
	echo "COLLECTOR_OIDC_CLIENT_SECRET_FILE (see deploy/docker-compose/.env.example)." >&2
	exit 1
fi
mode="$(stat -c %a "$mount")"
case "$mode" in
*00) ;;
*)
	echo "collector refuses to start: the client secret file is readable by group or" >&2
	echo "others (mode $mode). Run \`chmod 0600\` on that host path and start again." >&2
	exit 1
	;;
esac
install -m 0600 -o collector -g collector "$mount" /run/collector/client-secret
chown collector:collector /data
export COLLECTOR_OIDC_CLIENT_SECRET_FILE=/run/collector/client-secret

# The search index's key (lot 3a), when there is one. Handled like the client
# secret — the operator's file is a bind mount of a host file its own account
# owns, so a fixed uid this image could pick would not own it; the entrypoint
# copies it for the `collector` account it drops to. Unlike the client secret
# there is no mode refusal here: that refusal exists because the client secret
# is what makes this process *this SSO's client*, while this key is at-rest
# encryption material for the deployment — the encryption itself is the
# gocryptfs mount of the state volume (spec §4.2–4.3), not this process — and
# a loose mode on a host-local file is not the same claim about identity. An
# absent file is legitimate, not a fault: no key means the index is off, and
# `/search` answers `503 index_not_configured` (spec §4.2) — a capability that
# will hold years of other people's words does not switch itself on by
# omission, and its absence must never be a startup refusal that would turn an
# optional capability into a failure of the mail.
index_key=/run/secrets/collector-index-key
if [ -s "$index_key" ]; then
	install -m 0600 -o collector -g collector "$index_key" /run/collector/index-key
	export COLLECTOR_INDEX_KEY_FILE=/run/collector/index-key
elif [ -f "$index_key" ]; then
	# A key file that exists but is empty is not a key: the index would open
	# on nothing. Handled like the OIDC block's refusal of an absent key —
	# warn, and stay OFF (no key, no index) rather than start up on a file
	# that cannot encrypt anything.
	echo "collector: COLLECTOR_INDEX_KEY_FILE names an empty file, so the search" >&2
	echo "index stays OFF (no key, no index). Write a key into it and start again." >&2
	export COLLECTOR_INDEX_KEY_FILE=
elif [ -d "$index_key" ]; then
	# A path named in COLLECTOR_INDEX_KEY_FILE that did not exist became a
	# directory instead of a mount — a typo in `.env`. compose has already
	# put that directory's path in COLLECTOR_INDEX_KEY_FILE, and the binary
	# would refuse to start trying to read a directory as a key; the index
	# is optional, so blank the variable and let the mail flow — warned,
	# not hidden, because silence would leave the operator believing they
	# had turned search on.
	echo "collector: COLLECTOR_INDEX_KEY_FILE names a path that does not exist on the" >&2
	echo "host, so Docker mounted a directory at $index_key. The search index stays OFF" >&2
	echo "(no key, no index). Check the path in .env and start again." >&2
	export COLLECTOR_INDEX_KEY_FILE=
fi

exec setpriv --reuid=collector --regid=collector --clear-groups /usr/local/bin/twalk-collector "$@"
