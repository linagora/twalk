#!/usr/bin/env python3
"""Which suites a change has to run.

`suites.json` is the rule and this is the only thing that reads it: `plan.py`
(which the workflows call) imports `select` from here, and `test_selection.py`
checks the table against the repository through the same functions. One
implementation, so a workflow cannot quietly disagree with the table.

Also usable by hand, to answer "what would CI run for this branch?":

    git diff --name-only origin/main...HEAD | python3 .github/ci/routing.py

It says *why* each suite was selected, not only that it was, because "the
Gateway suite ran" and "the Gateway suite ran because tests/harness changed" are
not the same sentence to somebody reading a log.

Stdlib only, Python 3.9+: this runs before anything is installed.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
TABLE = HERE / "suites.json"


def load(path: Path = TABLE) -> dict:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def matches(trigger: str, changed: str) -> bool:
    """Does one trigger claim one changed path?

    A trigger ending in `/` is a directory prefix; `*` claims everything;
    anything else is an exact file path. Deliberately not glob syntax: a
    routing rule that needs a `**` is a routing rule somebody will read wrong.
    """
    if trigger == "*":
        return True
    if trigger.endswith("/"):
        return changed == trigger.rstrip("/") or changed.startswith(trigger)
    return changed == trigger


def is_ignored(table: dict, changed: str) -> bool:
    return any(matches(entry, changed) for entry in table["ignored"])


def claimed_by(table: dict, changed: str) -> list[str]:
    """Every suite whose triggers claim this path, `*` triggers excluded.

    The `*` of the routing suite is excluded on purpose: it claims every path,
    so counting it would make every path claimed and the unclaimed-path
    safety net dead.
    """
    return sorted(
        name
        for name, suite in table["suites"].items()
        if any(t != "*" for t in suite["triggers"])
        and any(matches(t, changed) for t in suite["triggers"] if t != "*")
    )


def select(table: dict, changed_paths: list[str]) -> tuple[list[str], dict, list[str]]:
    """Returns (selected suite names, why each was selected, unclaimed paths)."""
    why: dict[str, list[str]] = {}
    unclaimed: list[str] = []

    for path in changed_paths:
        if is_ignored(table, path):
            continue
        hits = claimed_by(table, path)
        if not hits:
            unclaimed.append(path)
        for name in hits:
            why.setdefault(name, []).append(path)

    # Every change runs the `*` suites — the routing check itself.
    for name, suite in table["suites"].items():
        if "*" in suite["triggers"] and changed_paths:
            why.setdefault(name, []).append("(every change)")

    if unclaimed and table.get("unclaimed_policy") == "run_everything":
        for name in table["suites"]:
            why.setdefault(name, []).append(
                f"(no routing rule claims {unclaimed[0]}, so everything runs)"
            )

    return sorted(why), why, unclaimed


def matrix(table: dict, selected: list[str], tier: str | None) -> list[dict]:
    out = []
    for name in selected:
        suite = table["suites"][name]
        if tier and suite["tier"] != tier:
            continue
        out.append(
            {
                "id": name,
                "what": suite["what"],
                "workdir": suite["workdir"],
                "run": suite["run"],
                "needs": suite["needs"],
                "tier": suite["tier"],
            }
        )
    return out


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="what CI would run for these paths")
    parser.add_argument(
        "paths",
        nargs="*",
        help="changed paths; read from stdin, one per line, when none are given",
    )
    parser.add_argument("--tier", choices=["required", "advisory"])
    args = parser.parse_args(argv)

    changed = args.paths or [line.strip() for line in sys.stdin if line.strip()]
    table = load()
    selected, why, unclaimed = select(table, changed)

    print(f"{len(changed)} changed path(s)")
    if unclaimed:
        print(
            "no routing rule claims "
            + ", ".join(unclaimed[:10])
            + " — selecting every suite"
        )
    shown = 0
    for name in selected:
        suite = table["suites"][name]
        if args.tier and suite["tier"] != args.tier:
            continue
        shown += 1
        reasons = ", ".join(sorted(set(why[name]))[:6])
        print(f"  {name} ({suite['tier']}, {suite['workdir']}) <- {reasons}")
        print(f"      {suite['run']}")
    if not shown:
        print("  nothing to run")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
