# Continuous integration

Verification that runs on a pull request and is a condition of merging it, rather than a habit (issue #183). Everything it does is decided by `.github/ci/suites.json` and checked by `.github/ci/test_selection.py`; the workflows hold no suite's name and no suite's command.

## What a contributor should expect

Open a pull request and it gets a verdict without anybody running anything by hand. Three checks decide it, and there are three rather than one because two facts must not share one signal:

- **`routing`** — the routing table checked against the repository. Always runs, hosted, under a second.
- **`verified`** — every selected suite that needs no Docker: the Python SDK, the shared harness's units, the consent cache's, the collector's OIDC seam against the harness's fake SSO, the Companion's Node-only suite. Always reports, including on a change that selects nothing.
- **`verified-stack`** — every selected suite that needs a Docker host: the Sensor, the Gateway, Hermes, the clerk, the collector at its process boundary. It **fails** when such suites were selected and `vars.TWALK_STACK_RUNNER` names no runner, because a green tick that meant "those never ran" would be the defect this project keeps shipping.

Running the suites locally is still the fastest way to find out whether your change works, and `AGENTS.md` holds the commands. What changed is that it is no longer the *verification*: a local run says the suites passed in your worktree, and `verified` says they passed on the change as it will land.

Two things are worth knowing before you read a red check.

**Not everything runs on every pull request.** The suites that raise the whole reference deployment — Synapse, NATS, four mautrix bridges, the Sensor, the Gateway, Hermes and a persona container — are the `advisory` tier. They do not block a merge, they run nightly on `main`, and they run on your pull request if you add the **`ci:deployment`** label. Add it when you touch `deploy/docker-compose/`, `bridges/`, or anything whose failure would only show up in the whole loop. The pull request that introduced CI argues why they are advisory and says plainly what that leaves uncovered.

**A flake is not silently retried into a pass.** The suites with an open flake ticket live only inside `companion-e2e-stack`, which is advisory. No suite with an open flake ticket is in the required tier, and none is promoted into it while that ticket is open. So a required check going red means something: it is your change, or it is a new flake worth a ticket of its own. Do not re-run a required check until it passes — if it is a flake, that is a finding.

The ticket that is still open there is **#148**, and what remains of it is not a race. Its own account of the three failures it was filed for ends with the one that "may not be flakiness at all": a screen that reads `GET /api/bridges` once on mount shows a transient failure as a permanent `unknown` — to a *user*, not only to a test. That is a finding about `companion/src/lib/networks/`, and it is what stands between `companion-e2e-stack` and the required tier now.

**#186 and #200 are closed**, and between them they were most of it. #186 was the `session` project, whose Gateway issues a device token that lives seconds so a browser can be watched losing a session: it raced by construction, because the client renews with a fifth of the lifetime in hand and a five-second lifetime therefore left one second of headroom — less than a timer slips on a machine that is compiling something, and that machine was this suite, which ran `session` in parallel with the project that builds and runs a Sensor. The lifetime is now fifteen seconds and the tolerance is three, stated in the suite; ordering the project last was tried and undone, because measured it passes at load 25–30 and a dependency chain would silence it whenever an earlier project went red; a credential's death is asked of the Gateway rather than slept for; and the project has a test budget of its own instead of the default thirty seconds, which had been carrying an eighteen-second deliberate wait. #200 was the `consent` project's failing spec, and it was the test rather than the screen — it asked whether a contact was waiting for a decision without naming the network, on a shared bus where the same account writes on two.

## What runs when

The rule that matters is the one two red-`main` incidents taught: **a change to a shared path runs the consumers' suites, not only its own.** `tests/harness/` is a dev dependency of `sensor/`, `hermes/`, `companion-gateway/`, `clerk/` and `collector/`, and the Companion's real-stack e2e brings up its compose file; `contracts/cloudevents/v1/` is read by the harness's validator, by the Sensor and by the Python SDK's tests; `companion-gateway/openapi.yaml` is what the Companion's API client is generated from, what the Sensor's stub Gateway transcribes an endpoint of, and what the clerk's refusal table is checked against; and `companion/src/lib/i18n/` — the Companion's catalogues — is embedded by the clerk with `include_str!`, so that a Gateway refusal answered on Buzz is the sentence the approval screen would show (#284).

That rule is not a comment in a workflow. `.github/ci/test_selection.py` **derives** each shared path's consumers from the repository — Cargo path dependencies, code that names a contract file, code that brings up the harness's stack — and fails when `suites.json` does not route what the derivation found. Add another consumer of the harness and the routing check goes red until the table says so. It also fails when a Rust test file exists that no suite runs, and when a tracked path is claimed by no suite and ignored by nothing.

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
| `gateway` | required | **~170 s**, 131 tests (+5 since #275: one integration, four unit, on the connection statuses; +3 integration and +3 unit since #281: the free/busy read, `hermes_freebusy` 20 s, one of them running the skill's script) | +3.0 GB target | 184 MB | warm stack |
| `hermes` | required | **132 s** | +2.0 GB target | 87 MB | warm stack |
| `clerk` | required | **not measured yet** (#265) | — | — | warm stack plus a Buzz relay, Postgres and Redis of its own; measure it before trusting the "full required run" figure below |
| `collector` | required | 31 unit + 5 + 3 tests, 0.5 s warm after the build (measured 2026-09-20); the build shares nothing with the other crates' targets | +1.6 GB target | — | no Docker: the fake SSO, side service and JMAP server run in-process |
| `collector-stack` | required | **78 s**, 16 tests — 12.8 s `calendar`, 9.4 s `freebusy`, 10.3 s `mail`, 25.2 s `push`, 10.5 s `reply`, 9.8 s `status` (measured 2026-09-20; ~180 s before #280 taught the tests to read from the bus head noted at their start) | — | — | warm stack; the process boundary against the bus, every poll at 1 s — except `push` (#277), which polls at 12 s to prove a delivery arrives inside the interval and waits one interval out with the socket cut |
| `sensor-deployment` | advisory | 61 s, 3 tests | small | 634 MB | **warm images** |
| `gateway-deployment` | advisory | 56 s, 4 tests | small | 305 MB | **warm images** |
| `hermes-full-loop` | advisory | 39 s | small | 153 MB | **warm images**; +10 containers |
| `clerk-deployment` | advisory | 47–61 s, 1 test (2 scenarios) | small | — | **warm images** (the Gateway's and the clerk's), the clerk test relay warm; needs passwordless `sudo` on the runner (#284) |
| `collector-deployment` | advisory | **40 s** warm, 1 test (measured 2026-09-21; ~75 s with the collector's image to build) | small | — | its own compose project (`twalk-collector-deploy-test`, NATS on 17608), `nats` and `collector` only — no Synapse, no Gateway, no SSO; the image built per stack (#279) |
| `companion-e2e-stack` | advisory | **160 s** — 112 passed, **1 failed** (#211), twice identically | +4.9 GB (Gateway and Sensor builds) | 3 869 MB | **warm** Gateway and Sensor builds; 277 s when it built both from cold, which is what the original figure was |

**A full required run is about 11½ minutes**, warm, when a change to `contracts/` or `tests/harness/` selects all three stack suites and they run one at a time: 383 + 163 + 132 s, with the hosted suites finishing inside 30 s alongside. A change to one component alone is 2–6½ minutes. The clerk's own stack variables are already in `.github/ci/stack-env.sh`, at `18370`, ahead of its suite being measured.

**Standing costs on the host**, so a runner can be sized: `matrixdotorg/synapse:v1.161.0` 517 MB, the four `dock.mau.dev/mautrix/*:v26.09` images 1 173 MB, `nats:2` 25 MB, the four locally built `twalk/*:local` images 798 MB, Playwright's Chromium and headless shell 654 MB per version, and the Rust target directories above. **`CARGO_PROFILE_DEV_DEBUG=line-tables-only`** is set on the hosted runners because the Sensor's `cargo build --tests` produces a **17.0 GB** target directory at this project's default debug settings and a hosted runner has about 14 GB; the reduced setting brings it to 6.0 GB and the cold build from 172 s to 94 s. It is deliberately *not* set on the stack runner, where a readable backtrace is worth the disk.

**Which figures are warm, and what that hides.** Every deployment-tier number above is a **warm BuildKit cache**: `deploy/docker-compose/sensor.Dockerfile` mounts `--mount=type=cache,id=twalk-sensor-target` and its own comment says the first build compiles the whole dependency tree in several minutes. On a runner that has never built these images, the deployment tier costs one release build per component on top of the table — minutes, not seconds. That is the main reason the tier is advisory, and the main argument for a persistent runner over an ephemeral one.

**Why hosted runners do not take the stack suites.** Not disk, once the debug setting is applied — the Sensor's 383 s is almost all Synapse round trips, Megolm and sync waits rather than CPU, so two cores do not multiply it. What two cores do is break it: pinned with `taskset -c 0,1`, the Sensor tier failed on `bridge_bots_are_not_contacts`' presence assertion, and chasing that down found the flake below.

**One required-tier test currently needs a warm stack.** `sensor/tests/bridge_bots_are_not_contacts.rs::a_bridge_bot_is_dropped_and_a_contact_in_the_same_room_is_published` times out waiting for a contact's presence event on a Synapse that has just been created (twice) and passes in 11 s on one that has already served a suite (four times). That is why `stack-teardown.sh` takes the deployment projects down with `down -v` and **leaves the shared test stack up**: tearing it down every run would make a required check red for a reason that is not the change under test. The comment in that script labels it as a workaround, and it stops being one when the test's ticket closes.

**The advisory tier was red when this was written, and that was the reason it was advisory.** One full `companion-e2e-stack` run, 277 s: 98 passed, one *flaky* and one *failed*. The flaky one was the `consent` project's "a contact who has written appears as awaiting a decision"; the failed one was the same project's "returning a contact to undecided is a decision, not an erasure", which failed on both attempts (`expect(received).toBe(false)`, received `true`), and eight later specs did not run behind it.

**Both were fixed by #200, and both were the test rather than the screen.** The cause is worth knowing because it will catch the next suite that reads this stack: the Companion's real-stack projects share one Synapse and one JetStream with the Sensor's and the Gateway's own suites, the Gateway's pending-contact projection replays that stream **from the beginning on every run**, and consent is keyed on `(subject, network)`. The consent journey asked "is this contact waiting for a decision?" without naming the network, about an account that is its own contact on `matrix` and a WhatsApp sender in half of `sensor/tests/` — so a correct `matrix` decision left a correct `whatsapp` row waiting and the flattened answer stayed `true`. Whether that suite passed therefore depended on whether the Sensor's suite had ever run against the stack, which is why one report said 52 specs passed and this one said one failed twice. The flaky sibling was the same cause from the other side: an event was matched by subject alone, so another suite's event about the same account — carrying the contract fixture's own `consent: granted` — could answer an assertion that *this* conversation's message is labelled `pending`.

Two things follow for anything else that runs here. **Name the network**, or the perimeter, or whatever the second half of the key is: a shared stack will eventually supply a row you did not put there. And **a test's own budget has to be larger than the waits inside it** — `approvals` opened every spec with a thirty-second poll inside Playwright's default thirty-second budget, so that poll could never use its window and the test died as a timeout saying nothing about what was slow. That was #186's `approvals` intermittent, and the `session` project had the same defect with an eighteen-second deliberate wait. Both projects now state a budget derived from the waits they contain.

**What is red in that suite today is #211**, found by running it repeatedly rather than once, and it is the same lesson a third time: the `portals` project builds portal rooms and never removes them, so the register — correctly — reads every run's rooms at once, and the project is green on its first run against a stack and red on every run after. That matters here more than it would elsewhere, because this project has *chosen* a persistent stack (see the required-tier test below), so "the first run" is not the configuration CI will have.

## Adding a suite

1. Add it to `.github/ci/suites.json`: what it is, its `component`, its `workdir`, its `run`, what it `needs`, its `tier`, and the paths that `trigger` it. A Rust suite also lists its `--test` targets.
2. Run `cd .github/ci && python3 -m unittest discover -s . -p 'test_*.py' -v`. It will tell you what you left out.
3. Nothing in `.github/workflows/` needs editing. If you find yourself editing a workflow to change what runs, the table is the thing to edit.
