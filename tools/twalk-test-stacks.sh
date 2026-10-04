#!/usr/bin/env bash
# What this host is holding for Twalk's tests, and what is safe to take away.
#
# Issue #128: a suite that could not bring its stack up reported as a suite that
# failed, and what an operator needs next is not `docker network ls` plus a
# naming convention recognised by eye. It is this — what exists, what each thing
# is for, which of them is provably dead, and one command for exactly those.
#
#     tools/twalk-test-stacks.sh                 # what is here
#     tools/twalk-test-stacks.sh --remove-stale  # and take the dead ones away
#     tools/twalk-test-stacks.sh --around 'cargo test --test deployment'
#     tools/twalk-test-stacks.sh --recreate twalk-sensor-test   # start one over
#
# `--recreate` is the operator's half of #432. The harness sweeps the shared
# stack's devices by itself, and that is all it is allowed to do: the stack is
# shared by every session on this host, so tearing it down is a decision with
# somebody else's suite on the other side of it. It is therefore here, with the
# project named in full, what is about to be destroyed printed first, and a
# confirmation — and never in `ensure_stack`.
#
# `--around` is the honest form of "a run leaves the host as it found it": it
# counts the networks and the projects, runs the command, counts again, and says
# what appeared — then exits with **the command's own status**. An assertion
# inside a suite could be skipped by the suite; this cannot, and it is also where
# a piped `cargo test` stops lying about its exit code, which is how the first
# failing run of #127 was reported as `exit code 0`.
#
# **Dead is proven, never assumed.** A per-run stack carries the pid of the
# `cargo test` that made it (`h21-<test>-<pid>-<nanos>`, `h23-…`), so this script
# asks the kernel whether that process still exists. Everything else is listed
# and left alone, including a name this file has never heard of: the first
# version of this script called such a name stale and offered to remove a
# parallel session's warm Synapse that had been up for eight days.
set -uo pipefail

# --- what a run left behind -------------------------------------------------

networks_now() {
  docker network ls --format '{{.Name}}' 2>/dev/null | grep -c . || echo 0
}

projects_now() {
  docker compose ls -a --format json 2>/dev/null \
    | python3 -c 'import json,sys
try: rows = json.load(sys.stdin)
except Exception: rows = []
for row in rows: print(row.get("Name", ""))' | sort
}

if [ "${1:-}" = "--around" ]; then
  shift
  [ $# -gt 0 ] || { echo "--around needs a command to run" >&2; exit 2; }
  before_networks=$(networks_now)
  before_projects=$(projects_now)
  echo "before: $before_networks Docker networks"
  echo
  bash -c "$*"
  ran=$?
  echo
  after_networks=$(networks_now)
  after_projects=$(projects_now)
  echo "after:  $after_networks Docker networks (was $before_networks)"
  appeared=$(comm -13 <(printf '%s\n' "$before_projects") <(printf '%s\n' "$after_projects") | grep -v '^$' || true)
  if [ -n "$appeared" ]; then
    echo "these compose projects appeared and are still here:"
    printf '  %s\n' $appeared
    echo "(`basename "$0"` with no arguments says which of them is provably dead)"
  elif [ "$after_networks" -eq "$before_networks" ]; then
    echo "the host is as the run found it."
  else
    echo "no compose project appeared, but the network count changed: something"
    echo "created a network outside compose, or removed one this run did not."
  fi
  # The command's status and not this script's last pipeline: a red suite that
  # announces itself green is the defect #128 names at the end.
  exit $ran
fi

# The long-lived projects, each with what it is — because a list of names nobody
# can explain is how one of these gets deleted by hand.
declare -A KNOWN=(
  [twalk]="the reference deployment — the owner's own, possibly in production"
  [twalk-public]="the reference deployment's published half"
  [twalk-test]="the shared harness stack (Synapse + NATS) that ensure_stack() brings up"
  [twalk-sensor-test]="the shared harness stack under its default name"
  [twalk-deploy-test]="the deployment suites' stack, shared by two suites on purpose (#212)"
  [twalk-bridges-test]="the bridge suites' stack"
  [twalk-nobridges-test]="the no-bridges deployment stack"
  [twalk-portals-test]="the portal register's stack"
  [twalk-clerk-test]="the clerk's Buzz relay, Postgres and Redis"
  [twalk-clerk-dev-bus]="the clerk's development bus"
  [twalk-clerk-deploy-test]="the clerk's deployment stack"
  [twalk-collector-deploy-test]="the collector's deployment stack"
  [twalk-ci-test]="CI's shared harness stack"
  [twalk-ci-deploy]="CI's deployment stack"
)

# --- starting a stack over, on purpose --------------------------------------

# The two projects that are not test stacks at all. The reference deployment on
# this host may be serving the owner's own messages, and `down -v` on it would
# take its Synapse database with it: naming one of these is a mistake, not a
# choice, so it is refused rather than confirmed.
declare -A NEVER_RECREATE=(
  [twalk]="the reference deployment — possibly in production on this host"
  [twalk-public]="the reference deployment's published half"
)

if [ "${1:-}" = "--recreate" ]; then
  project="${2:-}"
  [ -n "$project" ] || {
    echo "--recreate needs the compose project to start over, spelled out:" >&2
    echo "  $(basename "$0") --recreate twalk-sensor-test" >&2
    echo "(the shared harness stack's name is in the harness's own output, and" >&2
    echo " TWALK_TEST_STACK overrides it)" >&2
    exit 2
  }
  if [ -n "${NEVER_RECREATE[$project]:-}" ]; then
    echo "Refusing: $project is ${NEVER_RECREATE[$project]}." >&2
    echo "This command destroys a project's volumes. That one is not a test stack." >&2
    exit 2
  fi

  echo "About to destroy the compose project $project and its volumes."
  echo
  volume="${project}_synapse-data"
  created=$(docker volume inspect "$volume" --format '{{.CreatedAt}}' 2>/dev/null || true)
  [ -n "$created" ] && echo "  $volume has held data since $created"
  containers=$(docker ps -a --filter "label=com.docker.compose.project=$project" \
      --format '{{.Names}} ({{.Status}})' 2>/dev/null | sed 's/^/    /' || true)
  if [ -n "$containers" ]; then
    echo "  containers:"
    printf '%s\n' "$containers"
  else
    echo "  no container of that project is on this host — check the name"
  fi
  # What is lost, from the homeserver's own database rather than from a guess.
  if docker exec "${project}-synapse-1" test -f /data/homeserver.db 2>/dev/null; then
    docker exec "${project}-synapse-1" python3 -c '
import sqlite3
db = sqlite3.connect("file:/data/homeserver.db?mode=ro", uri=True)
one = lambda sql: db.execute(sql).fetchone()[0]
print("  the homeserver holds %d account(s), %d room(s), %d device(s), %d event(s)"
      % (one("select count(*) from users"), one("select count(*) from rooms"),
         one("select count(*) from devices"), one("select count(*) from events")))' 2>/dev/null || true
  fi
  echo
  echo "Another session on this host may be running a suite against it right now —"
  echo "this stack is shared, which is why nothing recreates it by itself (#432)."
  echo "The next suite to run will bring it up again, empty, and provision it."
  echo
  echo "And one suite is known to fail on a homeserver that has never served a run:"
  echo "sensor/tests/bridge_bots_are_not_contacts.rs — its presence assertion passes"
  echo "on a Synapse that has already answered a suite (measured twice each way, see"
  echo "CONTRIBUTING.md). After recreating, run it a second time before believing it."
  echo

  if [ "${3:-}" != "--yes" ]; then
    printf 'Type the project name to confirm: '
    read -r typed
    if [ "$typed" != "$project" ]; then
      echo "Not confirmed ($typed); nothing was touched."
      exit 1
    fi
  fi

  docker compose -p "$project" down -v --remove-orphans --timeout 30
  # `down -v` reconstructs the project from its containers' labels, so a project
  # whose containers were already gone leaves its volume behind. Said and taken
  # by name, because a volume left here is the accumulation this command exists
  # to end.
  if docker volume inspect "$volume" >/dev/null 2>&1; then
    echo "$volume outlived the project's containers; removing it by name."
    docker volume rm "$volume" >/dev/null || {
      echo "could not remove $volume — something still uses it:" >&2
      docker ps -a --filter "volume=$volume" --format '{{.Names}} ({{.Status}})' | sed 's/^/    /' >&2
      exit 1
    }
  fi
  echo "$project is gone. The next run of any suite brings it back."
  exit 0
fi

networks_total=$(docker network ls --format '{{.Name}}' 2>/dev/null | grep -c . || echo 0)
networks_ours=$(docker network ls --format '{{.Name}}' 2>/dev/null | grep -c '^\(twalk\|h21-\|h23-\)' || echo 0)
free=$(df -h --output=avail / 2>/dev/null | tail -1 | tr -d ' ')

echo "This host holds $networks_total Docker networks, $networks_ours of them Twalk's, and has $free free on /."
echo

projects=$(docker compose ls -a --format json 2>/dev/null \
  | python3 -c 'import json,sys
try: rows = json.load(sys.stdin)
except Exception: rows = []
for row in rows:
    name = row.get("Name", "")
    if name.startswith(("twalk", "h21-", "h23-")):
        print(name, row.get("Status", "?"), sep="\t")' | sort)

if [ -z "$projects" ]; then
  echo "No Twalk compose project on this host."
  exit 0
fi

# The pid a per-run project carries, or nothing. `h21-<test>-<pid>-<nanos>`.
run_pid() {
  printf '%s\n' "$1" | sed -n 's/^h2[13]-.*-\([0-9]\+\)-[0-9]\+$/\1/p'
}

stale=()
printf '%-44s %-26s %s\n' PROJECT STATUS WHAT
while IFS=$'\t' read -r name status; do
  [ -n "$name" ] || continue
  known=${KNOWN[$name]:-}
  pid=$(run_pid "$name")
  if [ -n "$known" ]; then
    what="$known"
  elif [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then
    what="DEAD — the run that made it (pid $pid) is gone"
    stale+=("$name")
  elif [ -n "$pid" ]; then
    what="a run in flight (pid $pid)"
  else
    what="not a name this script knows — left alone; look before you remove it"
  fi
  printf '%-44s %-26s %s\n' "$name" "$status" "$what"
done <<<"$projects"

echo
if [ ${#stale[@]} -eq 0 ]; then
  echo "Nothing provably dead. Every project above is either long-lived on purpose, a run in flight, or a name to look at by hand."
  exit 0
fi

echo "${#stale[@]} provably dead, and this is what removes them:"
for name in "${stale[@]}"; do
  echo "  docker compose -p $name down -v --remove-orphans"
done

if [ "${1:-}" = "--remove-stale" ]; then
  echo
  for name in "${stale[@]}"; do
    echo "removing $name"
    docker compose -p "$name" down -v --remove-orphans --timeout 30 >/dev/null 2>&1 || true
  done
  after=$(docker network ls --format '{{.Name}}' 2>/dev/null | grep -c . || echo 0)
  echo "networks: $networks_total before, $after after."
fi
