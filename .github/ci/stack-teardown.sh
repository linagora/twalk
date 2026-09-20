#!/usr/bin/env bash
# Leave the host as the run found it. Runs after every stack suite, passing or
# failing, which is the only version of this that works: a suite that tears down
# only on success leaves its stack behind exactly when somebody is about to
# re-run it.
#
# Three things get cleaned, and each is a defect this repository has on record:
#
#   - **The deployment projects**, with `down -v`. Volumes and not just
#     containers: a Synapse volume that survives carries the previous run's
#     accounts, and "the owner's account already exists" is not the failure the
#     next run is trying to report.
#   - **Whatever holds the Companion's ports.** #185: a `serve-like-gateway.mjs`
#     left listening on 4319 from the previous day made Playwright test a
#     day-old build and 36 tests failed as if the code were broken. A stale
#     server and broken code looked identical, which is this project's signature
#     defect.
#   - **The networks** those projects created. A Docker daemon's default address
#     pool runs out at around 31 networks on this project's reference host, and a
#     leaked compose network is one nothing will ever reclaim. Removed by name,
#     never `prune -a`, so nothing else on the host loses one.
#
# **The shared test stack is deliberately not torn down**, and that is a measured
# decision rather than an oversight. `tests/harness`'s stack is designed to stay
# up between runs — it is what makes a warm run take three seconds instead of
# fifteen — and one suite currently *needs* it warm:
# `sensor/tests/bridge_bots_are_not_contacts.rs`'s presence assertion timed out
# on a Synapse that had just been created (twice) and passed on one that had
# already served a suite (twice). Tearing it down after every run would make a
# required check red for a reason that is not the change under test, which is the
# exact failure mode this whole workflow exists to avoid.
#
# The clerk's relay stack (`twalk-ci-clerk`, `TWALK_CLERK_TEST_*`) persists
# between runs for the same reason: the clerk suite seeds it fresh — a real
# relay, Postgres and Redis — per run under a bus prefix of its own, so a warm
# relay carries nothing from the previous run for the next one to trip over.
# It is absent from the `projects` list below on purpose, same as the shared
# stack.
#
# Never `docker system prune`, never `docker builder prune`: on a self-hosted
# runner the daemon is shared with whatever else the host does, and the build
# cache is the only reason a warm run is warm.
set -uo pipefail

# The per-run stacks. `twalk-ci-test` — the shared harness stack — and
# `twalk-ci-clerk` — the clerk's relay stack — are absent from this list on
# purpose; see the header.
projects=(
  twalk-ci-deploy
  twalk-ci-bridges
  twalk-ci-nobridges
  twalk-ci-portals
  twalk-ci-loop
)

echo '--- the per-run deployment projects'
for project in "${projects[@]}"; do
  if docker compose -p "$project" ls -q 2>/dev/null | grep -q .; then
    echo "down -v $project"
  fi
  docker compose -p "$project" down -v --remove-orphans --timeout 30 >/dev/null 2>&1 || true
done

echo '--- the Companion'"'"'s origins'
for port in 18360 18361 18362; do
  holder=$(ss -lntpH "sport = :$port" 2>/dev/null | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2 || true)
  if [ -n "${holder:-}" ]; then
    echo "port $port was held by pid $holder — killing it"
    kill "$holder" 2>/dev/null || true
  fi
done

echo '--- the Gateway processes a real-stack run starts'
pkill -f 'serve-like-gateway.mjs' 2>/dev/null || true
pkill -f 'twalk-companion-gateway' 2>/dev/null || true

echo '--- per-stack images'
# Tagged per compose project (#38) so parallel worktrees never overwrite each
# other's build. The operator's own `:local` tags are never touched.
for project in "${projects[@]}"; do
  for image in twalk/sensor twalk/companion-gateway twalk/hermes twalk/persona-assistant; do
    docker image rm "$image:$project" >/dev/null 2>&1 || true
  done
done

echo '--- networks this run left behind'
for project in "${projects[@]}"; do
  docker network rm "${project}_default" >/dev/null 2>&1 || true
done

echo '--- what is left'
docker network ls --format '{{.Name}}' | wc -l | sed 's/^/networks on the daemon: /'
df -h / | tail -1
