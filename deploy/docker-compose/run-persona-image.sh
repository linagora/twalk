#!/bin/sh
# Runs a persona's container image with the environment the Hermes runtime
# gave THIS process. Shipped inside the deployment's Hermes image as
# `/usr/local/bin/run-persona`, and named in HERMES_PERSONAS as the argv of
# every persona the deployment hosts.
#
#   run-persona <container name> <image> [docker run option...]
#
# A persona ships as a container image (ADR 0008), and the runtime spawns a
# *process* whose environment it constructs (`hermes/src/environment.rs`).
# Something has to carry that environment across the container wall, and in
# a deployment that something is this script — the same seam
# `hermes/tests/run-persona-image.sh` fills for the test suite. The two are
# deliberately separate files: that one is a test fixture, this one ships in
# an image an operator runs, and each says what it is for. They agree on the
# two things that matter — every variable is forwarded, and SIGTERM becomes
# `docker stop` — and this one adds the network the deployment chose
# (ADR 0023).
#
# It forwards EVERY variable it was given, minus the handful it needs to run
# itself. That is deliberate: the runtime clears the environment before
# spawning this script (`Command::env_clear`), so what arrives here is the
# closed list ADR 0015 allows plus the operator's passthrough — and a
# wrapper that forwarded a fixed list of its own would make
# "the Companion Gateway's service token never reaches a persona" true by
# construction in the tests and false in the deployment.

set -eu

if [ "$#" -lt 2 ]; then
	echo "run-persona: usage: run-persona <container name> <image> [docker run option...]" >&2
	exit 64
fi

container=$1
image=$2
shift 2

# A previous run of the same persona may still be around — the runtime
# restarts a crashed persona, and `docker run --rm` has not necessarily
# finished cleaning up by then.
docker rm -f "$container" >/dev/null 2>&1 || true

# The deployment's network answer (ADR 0023), unless the persona's argv gave
# one of its own: the persona's container shares the host's network
# namespace, because that is where both endpoints it is pointed at live — the
# bus on the port the stack publishes on loopback, and an operator's LLM
# proxy, which on the reference host is `http://127.0.0.1:4000/v1`. A
# persona on a bridge network dials itself for the second and cannot reach
# the first by name either, since the runtime hands it one bus URL and that
# URL has to be true for both processes.
network_given=0
for argument in "$@"; do
	case "$argument" in
	--network | --network=* | --net | --net=*) network_given=1 ;;
	esac
done
if [ "$network_given" = 0 ]; then
	set -- --network host "$@"
fi

# Variables this script runs on rather than variables the runtime handed
# over for the persona. PATH is the default passthrough (a child needs it to
# `exec` at all); the rest is what a shell adds to its own environment, and
# DOCKER_HOST/XDG_RUNTIME_DIR are how an operator points this script at a
# rootless daemon — they configure the CLI, and belong to no persona.
forwarded=
for name in $(env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p'); do
	case "$name" in
	PATH | HOME | PWD | OLDPWD | SHLVL | HOSTNAME | DOCKER_HOST | XDG_RUNTIME_DIR | _) continue ;;
	esac
	forwarded="$forwarded --env $name"
done

# The runtime asks a persona to stop with SIGTERM, and it is this script that
# receives it. `docker run` does not reliably pass a signal on to the
# container it started, so the translation is explicit: `docker stop` sends
# the container's own process SIGTERM, which is what lets the SDK finish the
# event in flight and ack it. The timeout is below the runtime's default
# grace period, so the container is gone before the runtime gives up here.
stopping=0
stop() {
	stopping=1
	docker stop -t 5 "$container" >/dev/null 2>&1 || true
}
trap stop TERM INT

# shellcheck disable=SC2086 # $forwarded is a deliberately word-split list of
# --env flags, built above from names that match [A-Za-z_][A-Za-z0-9_]*.
docker run --rm --name "$container" $forwarded "$@" "$image" &
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
