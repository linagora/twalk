# Continuous integration

Verification that runs on a pull request and is a condition of merging it, rather than a habit (issue #183). Everything it does is decided by `.github/ci/suites.json` and checked by `.github/ci/test_selection.py`; the workflows hold no suite's name and no suite's command.

## What a contributor should expect

Open a pull request and it gets a verdict without anybody running anything by hand. Three checks decide it, and there are three rather than one because two facts must not share one signal:

- **`routing`** — the routing table checked against the repository. Always runs, hosted, under a second.
- **`verified`** — every selected suite that needs no Docker: the Python SDK, the shared harness's units, the Companion's Node-only suite. Always reports, including on a change that selects nothing.
- **`verified-stack`** — every selected suite that needs a Docker host: the Sensor, the Gateway, Hermes. It **fails** when such suites were selected and `vars.TWALK_STACK_RUNNER` names no runner, because a green tick that meant "those never ran" would be the defect this project keeps shipping.

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
- **`vars.TWALK_STACK_RUNNER`**, a self-hosted label: everything that raises a Synapse and a NATS. Until that repository variable is set, those jobs say what they did not run and print the command to type instead, and `verified-stack` goes red so nothing reads as verified that was not. One job at a time, `CARGO_BUILD_JOBS=4`, ports in 18300–18499, and `stack-teardown.sh` after every run whether it passed or failed — which takes the deployment stacks down with `down -v` and deliberately leaves the shared test stack up, because one Sensor suite fails against a Synapse that has just been created.

`.github/ci/stack-env.sh` is the single place the per-stack ports and compose projects are set. If you add a suite that raises a stack, its variables go there — and `AGENTS.md` already documents which variable moves which stack aside.

## What it costs

Measured on the project's reference host on 2026-09-19 — 20 cores, 62 GB RAM, Docker 29.5.2, `rustc 1.98.0-nightly`, Node 22.22.2 — not estimated. Wall clock is from `date +%s.%N` around each command with the exit code read from `${PIPESTATUS[0]}`; "peak memory" is the largest drop in `MemAvailable` sampled every three seconds, so it is the whole machine's extra usage rather than one process's RSS. `CARGO_BUILD_JOBS=4` throughout. **Re-measure these when a component grows a suite**; they are the numbers the tiering rests on, and a stale figure here is worse than none.

| suite | tier | wall clock | disk | peak memory | warm or cold |
|---|---|---|---|---|---|
| `routing` | required | 0.13 s local, 0.26 s hosted | — | — | no dependencies |
| `sdk-python` | required | 0.1 s | — | — | 70 tests, stdlib only |
| `harness` | required | 38.9 s cold, 1.9 s warm | +1 334 MB target | 934 MB | 8 tests |
| `companion` | required | 26.8 s, 26.7 s (two runs) | +212 MB `node_modules` | 3 128 MB | warm npm cache; `npm ci` alone 2.4 s |
| `sensor` | required | **383 s**, 62 tests | +6.0 GB target | 425 MB | warm stack. **67 s and red on a stack just created** — see below |
| `gateway` | required | **163 s**, 125 tests | +3.0 GB target | 184 MB | warm stack |
| `hermes` | required | **132 s** | +2.0 GB target | 87 MB | warm stack |
| `sensor-deployment` | advisory | 61 s, 3 tests | small | 634 MB | **warm images** |
| `gateway-deployment` | advisory | 56 s, 4 tests | small | 305 MB | **warm images** |
| `hermes-full-loop` | advisory | 39 s | small | 153 MB | **warm images**; +10 containers |
| `companion-e2e-stack` | advisory | **277 s** — 98 passed, 1 flaky, **1 failed** | +4.9 GB (Gateway and Sensor builds) | 3 869 MB | builds a Gateway *and* a Sensor from cold |

**A full required run is about 11½ minutes**, warm, when a change to `contracts/` or `tests/harness/` selects all three stack suites and they run one at a time: 383 + 163 + 132 s, with the hosted suites finishing inside 30 s alongside. A change to one component alone is 2–6½ minutes.

**Standing costs on the host**, so a runner can be sized: `matrixdotorg/synapse:v1.161.0` 517 MB, the four `dock.mau.dev/mautrix/*:v26.09` images 1 173 MB, `nats:2` 25 MB, the four locally built `twalk/*:local` images 798 MB, Playwright's Chromium and headless shell 654 MB per version, and the Rust target directories above. **`CARGO_PROFILE_DEV_DEBUG=line-tables-only`** is set on the hosted runners because the Sensor's `cargo build --tests` produces a **17.0 GB** target directory at this project's default debug settings and a hosted runner has about 14 GB; the reduced setting brings it to 6.0 GB and the cold build from 172 s to 94 s. It is deliberately *not* set on the stack runner, where a readable backtrace is worth the disk.

**Which figures are warm, and what that hides.** Every deployment-tier number above is a **warm BuildKit cache**: `deploy/docker-compose/sensor.Dockerfile` mounts `--mount=type=cache,id=twalk-sensor-target` and its own comment says the first build compiles the whole dependency tree in several minutes. On a runner that has never built these images, the deployment tier costs one release build per component on top of the table — minutes, not seconds. That is the main reason the tier is advisory, and the main argument for a persistent runner over an ephemeral one.

**Why hosted runners do not take the stack suites.** Not disk, once the debug setting is applied — the Sensor's 383 s is almost all Synapse round trips, Megolm and sync waits rather than CPU, so two cores do not multiply it. What two cores do is break it: pinned with `taskset -c 0,1`, the Sensor tier failed on `bridge_bots_are_not_contacts`' presence assertion, and chasing that down found the flake below.

**One required-tier test currently needs a warm stack.** `sensor/tests/bridge_bots_are_not_contacts.rs::a_bridge_bot_is_dropped_and_a_contact_in_the_same_room_is_published` times out waiting for a contact's presence event on a Synapse that has just been created (twice) and passes in 11 s on one that has already served a suite (four times). That is why `stack-teardown.sh` takes the deployment projects down with `down -v` and **leaves the shared test stack up**: tearing it down every run would make a required check red for a reason that is not the change under test. The comment in that script labels it as a workaround, and it stops being one when the test's ticket closes.

**The advisory tier is red today, and that is the reason it is advisory.** One full `companion-e2e-stack` run, 277 s: 98 passed, one *flaky* and one *failed*. The flaky one is the `consent` project's "a contact who has written appears as awaiting a decision", which failed and passed on Playwright's one CI retry — recorded as flaky rather than passed, which is the outcome the required tier deliberately does not accept. The failed one is the same project's "returning a contact to undecided is a decision, not an erasure", which failed on both attempts (`expect(received).toBe(false)`, received `true`) and so is a candidate defect rather than a flake. Eight later specs in that project did not run at all once it failed. If you are reading this because that suite is red, that is the state it was in when CI was built — check the ticket before assuming your change caused it.

## Adding a suite

1. Add it to `.github/ci/suites.json`: what it is, its `component`, its `workdir`, its `run`, what it `needs`, its `tier`, and the paths that `trigger` it. A Rust suite also lists its `--test` targets.
2. Run `cd .github/ci && python3 -m unittest discover -s . -p 'test_*.py' -v`. It will tell you what you left out.
3. Nothing in `.github/workflows/` needs editing. If you find yourself editing a workflow to change what runs, the table is the thing to edit.
