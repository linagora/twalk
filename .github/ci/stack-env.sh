#!/usr/bin/env bash
# The environment a stack suite runs under on a shared runner.
#
# Every suite in this repository can be moved aside — its compose project, its
# host ports, its bus subjects — and each component's test module documents the
# variables that do it (`AGENTS.md` collects them). This script sets all of them
# at once, into the 18300–18499 band, so a CI run never collides with a
# developer's own stack on the same host, or with the reference deployment the
# host may be running for real.
#
# Prints `NAME=value` lines for `$GITHUB_ENV`. Run it, do not source it:
#
#     bash .github/ci/stack-env.sh >>"$GITHUB_ENV"
#
# Nothing but `NAME=value` may be printed: `$GITHUB_ENV` rejects a line without
# an `=`, so every explanation below is a shell comment and stays out of the
# heredoc.
#
# The names are deliberately constant rather than derived from a run id. A
# per-run port would let two runs overlap, and overlapping is what the runner's
# single job slot exists to prevent: two concurrent stacks on one host is how a
# load average of 939 happened. One band, one run at a time, and
# `stack-teardown.sh` leaves nothing behind.
#
# CARGO_BUILD_JOBS is four, not however many cores the host has. The measured
# failure mode on this project's reference host was five concurrent Rust builds,
# a load average of 939, and 30 GB of swap exhausted — on a machine that also
# runs the owner's production services.
#
# TWALK_TEST_* is the shared test stack: sensor/, hermes/, companion-gateway/,
# and the Companion's real-stack e2e, which brings up the same compose file.
#
# TWALK_DEPLOY_TEST_* is the reference deployment, from the Sensor's side and the
# Gateway's; TWALK_BRIDGES_TEST_* is the same deployment with the four bridges
# up; TWALK_NO_BRIDGES_TEST_* is it with no bridge variable set at all (#172);
# TWALK_PORTALS_TEST_* is the portal register over it; TWALK_LOOP_TEST_* is the
# whole loop.
#
# TWALK_CLERK_TEST_* is the clerk's own test stack: a real Buzz relay (relay,
# Postgres, Redis) beside the shared Synapse/NATS one, seeded fresh per run
# under a bus prefix of its own, at 18370. TWALK_CLERK_DEPLOY_TEST_* is the
# reference deployment from the clerk's side (#284): the deployed Gateway and
# the deployed clerk container against that same relay, plus the clerk's own
# /health port, which is a host port because the clerk runs in the host's
# network namespace and 8084 is the reference deployment's own.
#
# The `_TEARDOWN` flags are deliberately **not** set, and since #199 that is a
# choice rather than a workaround. It used to be a workaround:
# `companion-gateway/tests/deployment.rs` called its own `teardown()` at the end
# of two of its three tests, and that teardown ran `compose rm -sfv
# companion-gateway sensor` — while the third test was still talking to the
# Gateway. With the flag on, measured: `Connection reset by peer (os error 104)`
# on `POST /api/bootstrap/rooms`. That suite now tears its stack down once,
# after its last scenario, so the flag is safe to set.
#
# CI still leaves it unset, because a suite's own teardown runs only when the
# suite passes — deliberately, so a failure leaves its containers and their logs
# to be read — and the run CI most needs the host back from is the failing one.
# `stack-teardown.sh` takes the whole compose project down with `down -v` after
# every run, passing or failing, which is strictly more thorough.
#
# TWALK_TEST_PORT and the two after it are the Companion's origins. 4319 and
# +1/+2 are the defaults, and a `serve-like-gateway.mjs` left on 4319 from a
# previous day is how #185 made a day-old build fail 36 tests as though the code
# were broken. CI moves them, and `stack-teardown.sh` kills whatever holds them
# at the end of every run.
set -euo pipefail

cat <<'ENV'
CARGO_BUILD_JOBS=4
TWALK_TEST_STACK=twalk-ci-test
TWALK_TEST_SYNAPSE_PORT=18300
TWALK_TEST_NATS_PORT=18301
TWALK_DEPLOY_TEST_STACK=twalk-ci-deploy
TWALK_DEPLOY_TEST_SYNAPSE_PORT=18310
TWALK_DEPLOY_TEST_NATS_PORT=18311
TWALK_DEPLOY_TEST_GATEWAY_PORT=18312
TWALK_BRIDGES_TEST_STACK=twalk-ci-bridges
TWALK_BRIDGES_TEST_SYNAPSE_PORT=18320
TWALK_BRIDGES_TEST_NATS_PORT=18321
TWALK_BRIDGES_TEST_GATEWAY_PORT=18322
TWALK_BRIDGES_TEST_WHATSAPP_PORT=18323
TWALK_BRIDGES_TEST_SIGNAL_PORT=18324
TWALK_BRIDGES_TEST_GMESSAGES_PORT=18325
TWALK_BRIDGES_TEST_TELEGRAM_PORT=18326
TWALK_NO_BRIDGES_TEST_STACK=twalk-ci-nobridges
TWALK_NO_BRIDGES_TEST_SYNAPSE_PORT=18330
TWALK_NO_BRIDGES_TEST_NATS_PORT=18331
TWALK_PORTALS_TEST_STACK=twalk-ci-portals
TWALK_PORTALS_TEST_SYNAPSE_PORT=18340
TWALK_PORTALS_TEST_NATS_PORT=18341
TWALK_PORTALS_TEST_GATEWAY_PORT=18342
TWALK_LOOP_TEST_STACK=twalk-ci-loop
TWALK_LOOP_TEST_SYNAPSE_PORT=18350
TWALK_LOOP_TEST_NATS_PORT=18351
TWALK_LOOP_TEST_GATEWAY_PORT=18352
TWALK_CLERK_TEST_STACK=twalk-ci-clerk
TWALK_CLERK_TEST_RELAY_PORT=18370
TWALK_CLERK_DEPLOY_TEST_STACK=twalk-ci-clerk-deploy
TWALK_CLERK_DEPLOY_TEST_SYNAPSE_PORT=18380
TWALK_CLERK_DEPLOY_TEST_NATS_PORT=18381
TWALK_CLERK_DEPLOY_TEST_GATEWAY_PORT=18382
TWALK_CLERK_DEPLOY_TEST_CLERK_PORT=18383
TWALK_TEST_PORT=18360
TWALK_TEST_BRIDGE_PORT=18361
TWALK_TEST_SESSION_PORT=18362
ENV
