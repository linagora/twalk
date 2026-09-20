#!/usr/bin/env python3
"""The routing table, checked against the repository rather than against itself.

Issue #183 asks for a *test* that a change to `contracts/` or `tests/harness/`
runs the consumers' suites, "rather than a comment asserting it". A comment
asserting it is what the two red-`main` incidents already had. So nothing here
reads a list of consumers out of `suites.json` and agrees with it: the
consumers are **derived from the repository's own dependency graph** — Cargo
path dependencies, and the code that reads a contract file or brings up the
harness's compose stack — and the table is required to cover what the
derivation found.

The derivation is deliberately a lower bound that over-includes. A file that
merely *names* a contract schema in a comment counts as a consumer, because the
two directions of error are not symmetrical: over-triggering costs a runner
minutes, and under-triggering is the defect this file exists to prevent.

    cd .github/ci && python3 -m unittest discover -s . -p 'test_*.py' -v

Stdlib only, no Docker, no Rust, under a second: this is the one check that can
be required on every pull request whatever else is skipped.
"""

from __future__ import annotations

import json
import re
import subprocess
import unittest
from pathlib import Path

import routing as selection  # this directory's routing.py

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent

CODE_SUFFIXES = {".rs", ".ts", ".mjs", ".js", ".py", ".sh", ".yaml", ".yml", ".toml", ".json", ".svelte"}


def tracked_files() -> list[Path]:
    """Every file git tracks, so an untracked scratch file cannot fail the suite."""
    out = subprocess.run(
        ["git", "-C", str(REPO), "ls-files", "-z"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [REPO / name for name in out.split("\0") if name]


def code_files() -> list[Path]:
    return [
        path
        for path in tracked_files()
        if path.suffix in CODE_SUFFIXES
        and not str(path.relative_to(REPO)).startswith("docs/")
        and not str(path.relative_to(REPO)).startswith(".github/")
    ]


def component_of(path: Path) -> str:
    """The top-level directory a file belongs to — the unit a suite is named for."""
    return path.relative_to(REPO).parts[0]


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:  # pragma: no cover - a tracked file that will not open
        return ""


# --------------------------------------------------------------------------
# The derivations: who actually consumes each shared path.
# --------------------------------------------------------------------------


def harness_consumers() -> set[str]:
    """Components that depend on `tests/harness/`.

    Two ways to depend on it, and both have bitten: a Cargo path dependency on
    the `twalk-test-harness` crate, and bringing up its `compose.test.yaml`
    (which is how `companion/tests/real-stack.mjs` depends on it without any
    Cargo relationship at all).
    """
    consumers: set[str] = set()
    for path in tracked_files():
        if path.name != "Cargo.toml":
            continue
        if component_of(path) == "tests":
            continue
        text = read(path)
        if re.search(r"twalk-test-harness\s*=.*path", text):
            consumers.add(component_of(path))
    for path in code_files():
        if component_of(path) == "tests":
            continue
        if "tests/harness/compose.test.yaml" in read(path):
            consumers.add(component_of(path))
    return consumers


def consent_cache_consumers() -> set[str]:
    """Components with a Cargo path dependency on `consent-cache/` (#273)."""
    consumers: set[str] = set()
    for path in tracked_files():
        if path.name != "Cargo.toml" or component_of(path) == "consent-cache":
            continue
        if re.search(r"twalk-consent-cache\s*=.*path", read(path)):
            consumers.add(component_of(path))
    return consumers


def contract_consumers() -> set[str]:
    """Components whose code names a file that really exists under `contracts/`.

    Both of the contract's directories: the CloudEvents schemas and fixtures,
    and the disclosure's sentences (`contracts/disclosure/`, #121), which the
    SDK, the Gateway and the Companion each read — a component that read only
    the sentences would otherwise be a consumer this derivation could not see.
    """
    consumers: set[str] = set()
    pattern = re.compile(r"contracts/(?:cloudevents|disclosure)/[A-Za-z0-9_./-]+")
    for path in code_files():
        if component_of(path) == "contracts":
            continue
        for reference in pattern.findall(read(path)):
            if (REPO / reference.rstrip(".,;:)\"'")).exists():
                consumers.add(component_of(path))
                break
    return consumers


def openapi_consumers() -> set[str]:
    """Components outside the Gateway whose code reads `companion-gateway/openapi.yaml`."""
    consumers: set[str] = set()
    for path in code_files():
        if component_of(path) == "companion-gateway":
            continue
        if "openapi.yaml" in read(path):
            consumers.add(component_of(path))
    return consumers


def catalogue_consumers() -> set[str]:
    """Components outside the Companion whose code names a catalogue file that
    really exists under `companion/src/lib/i18n/`.

    The clerk embeds the Companion's French and English catalogues with
    `include_str!` (#284), so that a Gateway refusal answered on Buzz is the
    sentence the approval screen would show — which means a sentence edited
    in the Companion has to rebuild and re-run the clerk, or the two doors
    onto one fact drift apart. The rule is `contract_consumers`'s, a
    reference to a file and not to the directory: the Hermes runtime and the
    SDK name the directory in a doc comment as where the five languages come
    from, and that is not a dependency on what a sentence says.
    """
    consumers: set[str] = set()
    pattern = re.compile(r"companion/src/lib/i18n/[A-Za-z0-9_.-]+")
    for path in code_files():
        if component_of(path) == "companion":
            continue
        for reference in pattern.findall(read(path)):
            if (REPO / reference.rstrip(".,;:)\"'")).is_file():
                consumers.add(component_of(path))
                break
    return consumers


DERIVATIONS = {
    "tests/harness/": harness_consumers,
    "consent-cache/": consent_cache_consumers,
    "contracts/": contract_consumers,
    "companion-gateway/openapi.yaml": openapi_consumers,
    "companion/src/lib/i18n/": catalogue_consumers,
}


class TheTable(unittest.TestCase):
    """`suites.json` has to be well formed before anything else means anything."""

    @classmethod
    def setUpClass(cls):
        cls.table = selection.load()
        cls.suites = cls.table["suites"]

    def test_every_suite_states_the_same_fields(self):
        required = {"what", "component", "workdir", "run", "needs", "tier", "triggers"}
        for name, suite in self.suites.items():
            with self.subTest(suite=name):
                self.assertTrue(
                    required <= set(suite),
                    f"{name} is missing {sorted(required - set(suite))}",
                )
                self.assertIn(suite["tier"], {"required", "advisory"})
                self.assertTrue(suite["triggers"], f"{name} is triggered by nothing")
                self.assertTrue(
                    (REPO / suite["workdir"]).is_dir(),
                    f"{name}'s workdir {suite['workdir']} does not exist",
                )

    def test_every_trigger_names_something_that_exists(self):
        """A rule for a path that was renamed away silently stops triggering."""
        for name, suite in self.suites.items():
            for trigger in suite["triggers"]:
                if trigger == "*":
                    continue
                with self.subTest(suite=name, trigger=trigger):
                    self.assertTrue(
                        (REPO / trigger.rstrip("/")).exists(),
                        f"{name} is triggered by {trigger}, which is not in the repository",
                    )

    def test_a_suite_is_triggered_by_its_own_component(self):
        for name, suite in self.suites.items():
            if suite["component"] == ".github":
                continue
            with self.subTest(suite=name):
                self.assertTrue(
                    any(
                        selection.matches(trigger, suite["workdir"] + "/x")
                        for trigger in suite["triggers"]
                    ),
                    f"{name} would not run on a change to {suite['workdir']} itself",
                )


class TheRuleTheRedMainIncidentsTaught(unittest.TestCase):
    """A change to a shared path runs the **consumers'** suites, not only its own.

    This is the acceptance criterion of #183 and the whole reason this file is
    here. The consumer sets are derived from the repository, so a fourth
    consumer of the harness — or a new component that reads the contract — makes
    this fail until `suites.json` routes it.
    """

    @classmethod
    def setUpClass(cls):
        cls.table = selection.load()

    def components_selected_by(self, path: str) -> set[str]:
        selected, _why, _unclaimed = selection.select(self.table, [path])
        return {
            self.table["suites"][name]["component"]
            for name in selected
            if self.table["suites"][name]["component"] != ".github"
        }

    def test_every_derived_consumer_is_routed(self):
        for shared, derive in DERIVATIONS.items():
            derived = derive()
            with self.subTest(shared=shared):
                self.assertTrue(
                    derived,
                    f"the derivation for {shared} found no consumers at all, which means "
                    "it has stopped working rather than that nothing depends on it",
                )
                probe = shared if not shared.endswith("/") else shared + "anything.txt"
                routed = self.components_selected_by(probe)
                missing = derived - routed
                self.assertFalse(
                    missing,
                    f"a change to {shared} would not run {sorted(missing)}'s suites, and "
                    f"{sorted(missing)} depend(s) on it. Add {shared} to those suites' "
                    "triggers in suites.json.",
                )

    def test_the_two_incidents_by_name(self):
        """The two cases that actually turned `main` red, spelled out."""
        harness = self.components_selected_by("tests/harness/src/stack.rs")
        for consumer in ("sensor", "hermes", "companion-gateway", "clerk", "collector"):
            self.assertIn(
                consumer,
                harness,
                f"a change to the shared harness must run {consumer}'s suite",
            )

        contract = self.components_selected_by(
            "contracts/cloudevents/v1/inbound.message.received.schema.json"
        )
        for consumer in (
            "sensor",
            "hermes",
            "companion-gateway",
            "clerk",
            "collector",
            "sdk",
            "tests",
        ):
            self.assertIn(
                consumer,
                contract,
                f"a change to the contract must run {consumer}'s suite",
            )

        # The contract's second directory (#121): three readers of one
        # table, each pinned to it by a test that would fail on a changed
        # sentence — so a changed sentence must run all three.
        sentences = self.components_selected_by("contracts/disclosure/v1/sentences.json")
        for consumer in ("sdk", "companion-gateway", "companion"):
            self.assertIn(
                consumer,
                sentences,
                f"a change to the disclosure's sentences must run {consumer}'s suite",
            )

    def test_a_component_only_change_does_not_run_the_others(self):
        """The filter has to be a filter, or the argument for having one is gone.

        `bridges/` is the narrowest real case in the repository: the bridges'
        base configurations are read by the four-bridge deployment test and by
        nothing else, so a change to one of them selects exactly one
        component's suites. If this ever selects more, either a dependency was
        added or a trigger was written too wide.
        """
        self.assertEqual(
            {"sensor"},
            self.components_selected_by("bridges/mautrix-whatsapp/config.yaml"),
        )

    def test_a_companion_change_does_not_run_the_sensor_or_hermes(self):
        """The Companion is downstream of the Gateway and upstream of nothing."""
        selected = self.components_selected_by("companion/src/routes/+page.svelte")
        self.assertNotIn("sensor", selected)
        self.assertNotIn("hermes", selected)
        self.assertIn("companion", selected)

    def test_an_openapi_change_runs_the_companions_client_check(self):
        self.assertIn(
            "companion",
            self.components_selected_by("companion-gateway/openapi.yaml"),
            "the Companion's API client is generated from openapi.yaml; a change "
            "there has to run `npm run api:check`",
        )

    def test_a_catalogue_or_openapi_change_runs_the_clerk(self):
        """The clerk speaks the Companion's sentences for the Gateway's codes (#284).

        `clerk/src/refusals.rs` embeds `companion/src/lib/i18n/{fr,en}.json` and
        checks the sentences it knows against `companion-gateway/openapi.yaml`
        in both directions, so a change to either file is a change to what the
        clerk says on Buzz — or to whether it still builds.
        """
        for shared in ("companion/src/lib/i18n/fr.json", "companion-gateway/openapi.yaml"):
            with self.subTest(shared=shared):
                self.assertIn(
                    "clerk",
                    self.components_selected_by(shared),
                    f"a change to {shared} has to run the clerk's suite",
                )


class EveryTestIsClaimed(unittest.TestCase):
    """No Rust integration test may exist that no suite runs.

    The suites name their `--test` targets explicitly, which is what lets the
    heavy deployment tests be a tier of their own. The cost of that is that a
    new test file is invisible to CI unless somebody lists it — so this is the
    check that makes adding one to `suites.json` compulsory.
    """

    @classmethod
    def setUpClass(cls):
        cls.table = selection.load()

    def test_every_rust_test_target_is_run_exactly_once(self):
        for component in ("sensor", "hermes", "companion-gateway", "clerk", "collector"):
            on_disk = {
                path.stem for path in sorted((REPO / component / "tests").glob("*.rs"))
            }
            claimed: list[str] = []
            for suite in self.table["suites"].values():
                if suite["component"] == component and "targets" in suite:
                    claimed.extend(suite["targets"])
            with self.subTest(component=component):
                self.assertEqual(
                    on_disk,
                    set(claimed),
                    f"{component}/tests/ and suites.json disagree: "
                    f"unrun {sorted(on_disk - set(claimed))}, "
                    f"named but absent {sorted(set(claimed) - on_disk)}",
                )
                self.assertEqual(
                    len(claimed),
                    len(set(claimed)),
                    f"{component} has a test target claimed by two suites",
                )

    def test_a_named_target_really_is_a_cargo_test_target(self):
        """`--test <name>` fails the whole run if no such target exists."""
        for name, suite in self.table["suites"].items():
            for target in suite.get("targets", []):
                with self.subTest(suite=name, target=target):
                    self.assertTrue(
                        (REPO / suite["workdir"] / "tests" / f"{target}.rs").is_file(),
                        f"{name} runs --test {target} and {target}.rs is not there",
                    )

    def test_one_suite_per_rust_component_runs_the_crate_s_own_unit_tests(self):
        """`cargo test --test X` runs X and nothing else — not the lib's units.

        Found by counting: `cargo test` in `sensor/` reports 188 tests and the
        sixteen `--test` targets account for 62 of them. The other 123 are the
        pure modules' own unit tests in `src/`, and naming targets explicitly had
        silently dropped every one of them. `--lib` puts them back, on exactly
        one suite per component so they are not run twice.
        """
        for component in ("sensor", "hermes", "companion-gateway", "clerk", "collector"):
            carriers = [
                name
                for name, suite in self.table["suites"].items()
                if suite["component"] == component and " --lib" in suite["run"]
            ]
            with self.subTest(component=component):
                self.assertEqual(
                    1,
                    len(carriers),
                    f"{component}: exactly one suite must run --lib, found {carriers}",
                )

    def test_the_run_command_names_the_targets_it_declares(self):
        """The `run` string and the `targets` list are one fact written twice."""
        for name, suite in self.table["suites"].items():
            if "targets" not in suite:
                continue
            with self.subTest(suite=name):
                self.assertEqual(
                    sorted(re.findall(r"--test (\S+)", suite["run"])),
                    sorted(suite["targets"]),
                    f"{name}'s run command and its targets list disagree",
                )


class NothingIsUnrouted(unittest.TestCase):
    """Every top-level path is either routed to a suite or explicitly ignored.

    The unclaimed-path fallback runs everything, which is the right failure
    direction and the wrong steady state: it would make the routing table
    pointless the day somebody adds a directory. So the fallback stays a safety
    net and this is the test that keeps it one.
    """

    @classmethod
    def setUpClass(cls):
        cls.table = selection.load()

    def test_every_tracked_file_is_accounted_for(self):
        """Not a sample of paths — every file git tracks, one by one.

        A per-directory probe would have missed `deploy/README.md`, which sits
        above the only part of `deploy/` any suite claims.
        """
        unaccounted = []
        for path in tracked_files():
            probe = str(path.relative_to(REPO))
            if selection.is_ignored(self.table, probe) or selection.claimed_by(
                self.table, probe
            ):
                continue
            unaccounted.append(probe)
        self.assertEqual(
            [],
            sorted(unaccounted)[:20],
            f"{len(unaccounted)} tracked path(s) are claimed by no suite and ignored by "
            "nothing, so a change to one of them would fall back to running every "
            "suite: add a trigger or add it to `ignored`",
        )

    def test_nothing_is_both_ignored_and_a_suites_component(self):
        components = {suite["component"] for suite in self.table["suites"].values()}
        for entry in self.table["ignored"]:
            if not entry.endswith("/"):
                continue
            with self.subTest(ignored=entry):
                self.assertNotIn(
                    entry.rstrip("/"),
                    components,
                    f"{entry} is ignored and also holds a suite's component",
                )

    def test_an_unclaimed_path_selects_everything_rather_than_nothing(self):
        selected, _why, unclaimed = selection.select(
            self.table, ["some-new-component/src/main.rs"]
        )
        self.assertEqual(["some-new-component/src/main.rs"], unclaimed)
        self.assertEqual(
            sorted(self.table["suites"]),
            selected,
            "a path no rule claims must select every suite, never none",
        )

    def test_a_prose_only_change_selects_only_the_routing_check(self):
        selected, _why, unclaimed = selection.select(self.table, ["AGENTS.md"])
        self.assertEqual([], unclaimed)
        self.assertEqual(["routing"], selected)


class TheWorkflowUsesThisTable(unittest.TestCase):
    """The workflows must get their job list from here and nowhere else.

    Checked textually rather than by parsing YAML, because parsing YAML would
    mean a dependency and this suite has none. It is a weaker check than the
    rest of the file and is honest about it: what it can prove is that the
    workflows call `select.py` and that no workflow hard-codes a suite's
    command.
    """

    @classmethod
    def setUpClass(cls):
        cls.table = selection.load()
        cls.workflows = {
            path.name: path.read_text(encoding="utf-8")
            for path in sorted((HERE.parent / "workflows").glob("*.yml"))
        }

    def test_there_is_a_pull_request_workflow_and_it_asks_this_table(self):
        self.assertIn("pull-request.yml", self.workflows)
        self.assertIn(".github/ci/plan.py", self.workflows["pull-request.yml"])
        self.assertIn(
            "import routing",
            (HERE / "plan.py").read_text(encoding="utf-8"),
            "plan.py must get its answer from routing.py, which reads suites.json",
        )

    def test_the_workflow_runs_the_routing_check_unconditionally(self):
        """A required check that can be skipped is a required check that can be
        skipped by the change that breaks it."""
        text = self.workflows["pull-request.yml"]
        self.assertIn(self.table["suites"]["routing"]["run"], text)
        self.assertIn("name: verified", text)
        self.assertIn("name: verified-stack", text)

    def test_no_workflow_hard_codes_a_suites_command(self):
        for filename, text in self.workflows.items():
            for name, suite in self.table["suites"].items():
                if name == "routing":
                    continue  # the routing job is the one thing a workflow may spell out
                with self.subTest(workflow=filename, suite=name):
                    self.assertNotIn(
                        suite["run"],
                        text,
                        f"{filename} spells out {name}'s command; it should come from "
                        "suites.json through select.py",
                    )


if __name__ == "__main__":
    unittest.main()
