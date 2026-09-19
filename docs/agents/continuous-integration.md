# Continuous integration

Verification that runs on a pull request and is a condition of merging it, rather than a habit (issue #183). Everything it does is decided by `.github/ci/suites.json` and checked by `.github/ci/test_selection.py`; the workflows hold no suite's name and no suite's command.

## What a contributor should expect

Open a pull request and it gets a verdict without anybody running anything by hand. **`verified`** is the check that decides: it is green when every suite your change selected passed, and it always reports, including on a change that legitimately selects nothing.

Running the suites locally is still the fastest way to find out whether your change works, and `AGENTS.md` holds the commands. What changed is that it is no longer the *verification*: a local run says the suites passed in your worktree, and `verified` says they passed on the change as it will land.

Two things are worth knowing before you read a red check.

**Not everything runs on every pull request.** The suites that raise the whole reference deployment — Synapse, NATS, four mautrix bridges, the Sensor, the Gateway, Hermes and a persona container — are the `advisory` tier. They do not block a merge, they run nightly on `main`, and they run on your pull request if you add the **`ci:deployment`** label. Add it when you touch `deploy/docker-compose/`, `bridges/`, or anything whose failure would only show up in the whole loop. The pull request that introduced CI argues why they are advisory and says plainly what that leaves uncovered.

**A flake is not silently retried into a pass.** The suites with open flake tickets (#148, #186 — the Companion's `session`, `networks` and `approvals` Playwright projects) live only inside `companion-e2e-stack`, which is advisory. No suite with an open flake ticket is in the required tier, and none is promoted into it while that ticket is open. So a required check going red means something: it is your change, or it is a new flake worth a ticket of its own. Do not re-run a required check until it passes — if it is a flake, that is a finding.

## What runs when

The rule that matters is the one two red-`main` incidents taught: **a change to a shared path runs the consumers' suites, not only its own.** `tests/harness/` is a dev dependency of `sensor/`, `hermes/` and `companion-gateway/`, and the Companion's real-stack e2e brings up its compose file; `contracts/cloudevents/v1/` is read by the harness's validator, by the Sensor and by the Python SDK's tests; `companion-gateway/openapi.yaml` is what the Companion's API client is generated from.

That rule is not a comment in a workflow. `.github/ci/test_selection.py` **derives** each shared path's consumers from the repository — Cargo path dependencies, code that names a contract file, code that brings up the harness's stack — and fails when `suites.json` does not route what the derivation found. Add a fourth consumer of the harness and the routing check goes red until the table says so. It also fails when a Rust test file exists that no suite runs, and when a tracked path is claimed by no suite and ignored by nothing.

A path nothing claims selects **every** suite. Wrong in the expensive direction only, and the check above is what keeps that a safety net rather than the normal case.

## Where it runs

Two kinds of runner, because the suites are not one kind of thing.

- **GitHub-hosted**, free and maintained by nobody here: the routing check, the Python SDK's units, the shared harness's units, and the Companion's Node-only suite. These need no Docker and no stack.
- **`vars.TWALK_STACK_RUNNER`**, a self-hosted label: everything that raises a Synapse and a NATS. Until that repository variable is set, those jobs report that they did not run, name the command to type instead, and do not block `verified` by hanging. One job at a time, `CARGO_BUILD_JOBS=4`, ports in 18300–18499, and `stack-teardown.sh` after every run whether it passed or failed.

`.github/ci/stack-env.sh` is the single place the per-stack ports and compose projects are set. If you add a suite that raises a stack, its variables go there — and `AGENTS.md` already documents which variable moves which stack aside.

## Adding a suite

1. Add it to `.github/ci/suites.json`: what it is, its `component`, its `workdir`, its `run`, what it `needs`, its `tier`, and the paths that `trigger` it. A Rust suite also lists its `--test` targets.
2. Run `cd .github/ci && python3 -m unittest discover -s . -p 'test_*.py' -v`. It will tell you what you left out.
3. Nothing in `.github/workflows/` needs editing. If you find yourself editing a workflow to change what runs, the table is the thing to edit.
