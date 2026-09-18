#!/bin/sh
# The deployment's Hermes entrypoint: start the runtime, or say plainly that
# this deployment has not been given a model to reason with and host nothing
# until it is.
#
# Twalk ships no LLM and there is no default endpoint (ADR 0015): the runtime
# refuses to start without `HERMES_LLM_BASE_URL` and `HERMES_LLM_MODEL`, and
# it is right to. But this service is part of the default stack, and an
# operator who runs Synapse, the bus, the Sensor and the Gateway without
# naming a model must still get a stack that comes up: a container exiting
# into a restart loop would make `docker compose up -d --wait` fail for a
# deployment that is working exactly as configured.
#
# So the refusal is stated here, once, in the log an operator reads — naming
# the variables that are missing — and the container stays up hosting
# nothing. That is the same shape the rest of the platform uses for "allowed
# to exist, handed nothing": a paused persona keeps running and receives no
# event (ADR 0013).
#
# Anything else — an endpoint that is wrong, a persona image that will not
# run, a bus that is not there — is the runtime's to report, and it is
# already better at it than a shell script would be.

set -eu

missing=
for name in HERMES_PERSONAS HERMES_LLM_BASE_URL HERMES_LLM_MODEL; do
	eval "value=\${$name:-}"
	[ -n "$value" ] || missing="$missing $name"
done

if [ -n "$missing" ]; then
	echo "hermes is hosting no persona. Unset in this deployment:$missing." >&2
	echo "Twalk ships no LLM and no default persona list: name an endpoint, a model" >&2
	echo "and a persona in .env (see deploy/docker-compose/.env.example), then" >&2
	echo "\`docker compose up -d hermes\`. Until then this container runs and hosts" >&2
	echo "nothing, and the rest of the stack is unaffected." >&2
	exec sleep infinity
fi

exec twalk-hermes "$@"
