#!/bin/sh
# Runs a persona's container image with the environment the Hermes runtime
# gave THIS process.
#
# A persona ships as a container image (ADR 0008, and the host has no pip),
# so the seam the Hermes suite drives is a container's process boundary. The
# runtime, though, spawns a process and constructs its environment
# (`hermes/src/environment.rs`) — so something has to carry that environment
# across the container wall, and that something is this script: the argv the
# runtime is configured with in tests.
#
# It forwards EVERY variable it was given, minus the handful it needs to run
# itself (below). That is deliberate: a test asserting that the Companion
# Gateway's service token never reaches a persona (ADR 0015) is only worth
# something if a leaked variable would actually show up in `docker inspect`.
# A wrapper that forwarded a fixed list would make that assertion true by
# construction and prove nothing.
#
#   run-persona-image.sh <container name> <image>

set -eu

container=$1
image=$2
shift 2

# A previous run of the same persona may still be around — the runtime
# restarts a crashed persona, and `docker run --rm` has not necessarily
# finished cleaning up by then.
docker rm -f "$container" >/dev/null 2>&1 || true

# Variables this script runs on rather than variables the runtime handed
# over for the persona. PATH and HOME belong to the host's shell and docker
# CLI; the rest is what a shell adds to its own environment.
set --
for name in $(env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p'); do
	case "$name" in
	PATH | HOME | PWD | OLDPWD | SHLVL | HOSTNAME | DOCKER_HOST | XDG_RUNTIME_DIR | _) continue ;;
	esac
	set -- "$@" --env "$name"
done

# The runtime asks a persona to stop with SIGTERM, and it is this script that
# receives it. `docker run` does not reliably pass a signal on to the
# container it started — whether it does depends on the CLI's attach mode,
# and on the hosts this suite runs on it does not — so the translation is
# explicit: `docker stop` sends the container's own process SIGTERM, which is
# what lets the SDK finish the event in flight and ack it. The timeout is
# below the runtime's default grace period, so the container is gone before
# the runtime gives up on this script.
stopping=0
stop() {
	stopping=1
	docker stop -t 5 "$container" >/dev/null 2>&1 || true
}
trap stop TERM INT

# Host networking, for the same reason `compose.assistant.yaml` uses it: the
# two endpoints the persona is pointed at — the bus on the port the test
# stack publishes, and the stub LLM inside the test process — are both on
# the host's loopback.
docker run --rm --name "$container" --network host "$@" "$image" &
run=$!

# A trapped signal makes `wait` return early. `stop` has already run — and
# `docker stop` blocks until the container is down — so when that happens the
# persona has stopped cleanly and this script has nothing left to report.
wait "$run"
status=$?
if [ "$stopping" = 1 ]; then
	wait "$run" >/dev/null 2>&1 || true
	exit 0
fi
exit "$status"
