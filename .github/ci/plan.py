#!/usr/bin/env python3
"""Turn a list of changed paths into the workflow's matrices.

The workflow calls this and nothing else: it holds no suite's name and no
suite's command, so the routing table in `suites.json` cannot be contradicted by
a job definition. `routing.py` decides *which* suites; this decides *where* each
one can run, which is a different question and the reason the two are separate
files:

  - a suite with no `docker` in its `needs` fits a GitHub-hosted runner, is free,
    and is required;
  - a suite that needs Docker needs a runner with the images and the room, so it
    goes to the stack matrix;
  - a suite of the `advisory` tier goes to its own matrix whatever it needs,
    because its result does not block a merge.

    python3 .github/ci/plan.py changed.txt

Writes `hosted=`, `stack=`, `advisory=` and an `any_*` flag for each to
`$GITHUB_OUTPUT`, a table to `$GITHUB_STEP_SUMMARY`, and the same table to
stdout so that a local run of this script says the same thing a CI run does.

Stdlib only.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import routing  # noqa: E402  (after the path insert, deliberately)


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__, file=sys.stderr)
        return 2

    changed = [line.strip() for line in Path(argv[0]).read_text().splitlines() if line.strip()]
    table = routing.load()
    selected, why, unclaimed = routing.select(table, changed)

    def entry(name: str) -> dict:
        suite = table["suites"][name]
        return {
            "id": name,
            "what": suite["what"],
            "workdir": suite["workdir"],
            "run": suite["run"],
            "needs": ",".join(suite["needs"]),
            "tier": suite["tier"],
            "why": ", ".join(sorted(set(why[name]))[:6]),
        }

    hosted: list[dict] = []
    stack: list[dict] = []
    advisory: list[dict] = []
    for name in selected:
        suite = table["suites"][name]
        if name == "routing":
            continue  # its own unconditional job in the workflow
        if suite["tier"] == "advisory":
            advisory.append(entry(name))
        elif "docker" in suite["needs"]:
            stack.append(entry(name))
        else:
            hosted.append(entry(name))

    lines = ["## What this change selected", ""]
    if unclaimed:
        lines += [
            f"No routing rule claims `{unclaimed[0]}`, so **every** suite was "
            "selected. Add a trigger in `.github/ci/suites.json`.",
            "",
        ]
    lines += ["| suite | tier | runner | selected because |", "|---|---|---|---|"]
    for where, items in (("hosted", hosted), ("stack", stack), ("stack", advisory)):
        for item in items:
            lines.append(f"| `{item['id']}` | {item['tier']} | {where} | {item['why']} |")
    if not (hosted or stack or advisory):
        lines.append("| _nothing_ | | | this change touches no tested path |")
    summary = "\n".join(lines) + "\n"

    print(summary)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as handle:
            handle.write(summary)

    # `all` is what the nightly run uses: one matrix, every selected suite, in
    # the order suites.json declares them rather than split by runner. The
    # nightly host is the stack runner, so the split has nothing to decide.
    everything = [entry(name) for name in selected if name != "routing"]

    outputs = []
    for key, items in (
        ("hosted", hosted),
        ("stack", stack),
        ("advisory", advisory),
        ("all", everything),
    ):
        payload = json.dumps({"include": items}, separators=(",", ":"))
        assert "\n" not in payload, "a suite's `what` must be one line"
        outputs.append(f"{key}={payload}")
        outputs.append(f"any_{key}={'true' if items else 'false'}")

    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as handle:
            handle.write("\n".join(outputs) + "\n")
    else:
        print("\n".join(outputs), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
