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
exec setpriv --reuid=collector --regid=collector --clear-groups /usr/local/bin/twalk-collector "$@"
