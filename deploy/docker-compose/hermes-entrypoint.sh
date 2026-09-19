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

# Since #184 the model may come from the **Companion Gateway** rather than from
# `.env`: the runtime reads `GET /api/settings/runtime` at startup and injects
# what it finds (ADR 0015). So the endpoint and the model are only required here
# when this deployment has no Gateway to read them from — and when it has one,
# the runtime is the better reporter of "the user has named no model yet": it
# starts, hosts nothing, says so, and starts the personas the moment a model is
# named, with no restart. This gate is left with the one thing no Gateway can
# ever supply.
required="HERMES_PERSONAS"
if [ -z "${HERMES_GATEWAY_URL:-}" ] || [ -z "${HERMES_GATEWAY_SERVICE_TOKEN:-}" ]; then
	required="$required HERMES_LLM_BASE_URL HERMES_LLM_MODEL"
fi

missing=
for name in $required; do
	eval "value=\${$name:-}"
	[ -n "$value" ] || missing="$missing $name"
done

if [ -n "$missing" ]; then
	echo "hermes is hosting no persona. Unset in this deployment:$missing." >&2
	echo "Twalk ships no LLM and no default persona list: name a persona in .env, and" >&2
	echo "either an endpoint and a model there or a Companion Gateway to read the one" >&2
	echo "the user chose from (see deploy/docker-compose/.env.example), then" >&2
	echo "\`docker compose up -d hermes\`. Until then this container runs and hosts" >&2
	echo "nothing, and the rest of the stack is unaffected." >&2
	exec sleep infinity
fi

exec twalk-hermes "$@"
