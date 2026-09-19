// Playwright is the authority for the Companion (spec #65): component tests
// are allowed for display logic, and anything touching crypto, the session or
// the Gateway goes through a real browser.
//
// Two choices the spec fixes:
//
//   - `channel: 'chromium'` — the new headless mode, which is the real browser
//     rather than the old headless shell. `localhost` is a secure context, so
//     no flags and no HTTPS are needed for `crypto.subtle`, IndexedDB or a
//     service worker.
//   - the built export, served by `tests/serve-like-gateway.mjs`, which
//     transcribes the Gateway's own resolution order. A server that resolves
//     paths differently would prove a routing contract nobody ships.
//
// The suite runs against `build/`, so `npm run build` comes first — `npm test`
// sequences them.

import { defineConfig, devices } from '@playwright/test';

const port = Number(process.env.TWALK_TEST_PORT ?? 4319);

/**
 * The second origin, for the network journeys of ticket #68. Same server, same
 * orchestrator, same proxy — a *second* Companion Gateway behind it, with the
 * stub bridge configured and an owner whose account already exists.
 *
 * Two origins because there are two owners, and that is not incidental: the
 * bootstrap journey (#67) has to start from an account that does **not** exist,
 * since the Gateway creates this deployment's one account and refuses a second.
 * A bridge login has to start from one that does. One Gateway cannot be both.
 */
const bridgePort = Number(process.env.TWALK_TEST_BRIDGE_PORT ?? port + 1);

/**
 * The third origin, for the session journeys of ticket #111.
 *
 * A third Gateway because the thing under test is the *deployment's* device
 * token lifetime: this one issues tokens that live five seconds, so a browser
 * can be watched losing a session and getting it back. Putting that TTL on the
 * bridge origin would have every other spec expiring mid-assertion.
 */
const sessionPort = Number(process.env.TWALK_TEST_SESSION_PORT ?? port + 2);

/**
 * `TWALK_TEST_REAL_STACK=1` puts a real Companion Gateway and a real Synapse
 * behind the test server (`tests/real-stack.mjs`), which is what the bootstrap
 * journey of ticket #67 needs: cross-signing, secret storage and a key backup
 * are round trips to a homeserver. It costs Docker and a `cargo build`, so the
 * default is off and the specs that need it skip themselves — `npm test` stays
 * a Node-only suite, and `npm run test:e2e:stack` is the full one.
 */
const realStack = process.env.TWALK_TEST_REAL_STACK === '1';

export default defineConfig({
	testDir: 'tests/e2e',
	fullyParallel: true,
	forbidOnly: !!process.env.CI,
	retries: process.env.CI ? 1 : 0,
	reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',

	use: {
		baseURL: `http://127.0.0.1:${port}`,
		trace: 'retain-on-failure'
	},

	projects: [
		{
			name: 'chromium',
			testIgnore: [
				'networks/**',
				'dashboard/**',
				'approvals/**',
				'consent/**',
				'portals/**',
				'session/**'
			],
			use: {
				...devices['Desktop Chrome'],
				channel: 'chromium',
				// The wireframes are mobile-first, 375–428 CSS px. Screen 1 is
				// asserted at the narrow end, where it has to work.
				viewport: { width: 390, height: 844 }
			}
		},
		{
			// Screens 3, 3a–3d and the management screen, against the real
			// Gateway and the stub bridge. These specs skip themselves without
			// the stack, like every other spec that needs it.
			//
			// One worker, for the reason the `dashboard` project gives below:
			// there is one Gateway and one stub bridge for the whole suite, and
			// **one login per bridge instance** (#55). Running these files at
			// once only appeared to work because each drove a different bridge;
			// as soon as two specs touch one bridge, a login started by either
			// is a `409 login_in_flight` for the other, and a login one of them
			// leaves behind is a link the picker then reports. Ordering them is
			// cheaper than teaching every spec to tolerate another's state
			// (#108).
			name: 'networks',
			testMatch: 'networks/**/*.spec.ts',
			fullyParallel: false,
			workers: 1,
			use: {
				...devices['Desktop Chrome'],
				channel: 'chromium',
				viewport: { width: 390, height: 844 },
				baseURL: `http://127.0.0.1:${bridgePort}`
			}
		},
		{
			// Screens 4 and 5 (ticket #69), on the same origin as the network
			// journeys: activating a persona needs a signed-in device, which
			// is the bridge Gateway's owner, and pausing one needs a bridge to
			// have been connected first.
			//
			// One worker, and after `networks`, because there is one Gateway
			// and one stub bridge for the whole suite: one consent journal,
			// one device list, one login per bridge instance. Two specs
			// flipping the same persona, or two completing a login on the same
			// bridge, would each be asserting the other's state — and a login
			// one of them starts is a `409 login_in_flight` for the other.
			//
			// `fullyParallel: false` orders the tests inside a file; `workers`
			// orders the files of this project; `dependencies` orders this
			// project against the one that drives the same bridge.
			name: 'dashboard',
			testMatch: 'dashboard/**/*.spec.ts',
			fullyParallel: false,
			workers: 1,
			dependencies: ['networks'],
			use: {
				...devices['Desktop Chrome'],
				channel: 'chromium',
				viewport: { width: 390, height: 844 },
				baseURL: `http://127.0.0.1:${bridgePort}`
			}
		},
		{
			// The approval screen (#100), on the same origin as the dashboard:
			// approving needs a signed-in device, a bus to publish a suggestion
			// on and the Gateway's own consent journal to read — which is the
			// bridge Gateway, the only one configured with all three.
			//
			// One worker and after `dashboard`, for the reason that project
			// gives: one Gateway, one consent journal, one bus. These specs
			// publish inbound events and write consent decisions about
			// contacts, which is state the dashboard's own counts are asserted
			// against — so they run when it has finished, not beside it.
			name: 'approvals',
			testMatch: 'approvals/**/*.spec.ts',
			fullyParallel: false,
			workers: 1,
			dependencies: ['dashboard'],
			use: {
				...devices['Desktop Chrome'],
				channel: 'chromium',
				viewport: { width: 390, height: 844 },
				baseURL: `http://127.0.0.1:${bridgePort}`
			}
		},
		{
			// The consent screen (#170), on the same origin as the approval
			// screen and for the same reasons: deciding about a contact needs a
			// signed-in device, a bus for the decision to be published on and
			// the Gateway's own consent journal and pending-contact projection —
			// which is the bridge Gateway, the only one configured with all of
			// them.
			//
			// One worker, and after `approvals`, because this project is the one
			// that starts a **real Sensor** (`tests/e2e/consent/sensor.ts`): the
			// Sensor's consent consumer is a durable with a constant name, so two
			// of them on one bus would split the consent stream between them. It
			// also writes consent decisions and makes a new contact pending,
			// which is state the dashboard's own counts are asserted against, so
			// it runs when those have finished rather than beside them.
			name: 'consent',
			testMatch: 'consent/**/*.spec.ts',
			fullyParallel: false,
			workers: 1,
			dependencies: ['approvals'],
			use: {
				...devices['Desktop Chrome'],
				channel: 'chromium',
				viewport: { width: 390, height: 844 },
				baseURL: `http://127.0.0.1:${bridgePort}`
			}
		},
		{
			// The conversation chooser (#143), on the bridge origin: it reads a
			// portal register, which needs a bridge configured with an appservice
			// token and the bot to read as, and it drives a real Sensor into a
			// real portal room.
			//
			// One worker, and after `consent` rather than beside it, for a reason
			// that is not about state: **that project starts a real Sensor too**,
			// and so does this one. The Sensor's consent consumer is a durable
			// with a constant name, so two of them on one bus split the consent
			// stream between them — which is why `sensor/tests/harness` locks
			// around every test that starts one, and why these two projects are
			// ordered rather than parallel. It borrows that project's own helper
			// (`tests/e2e/consent/sensor.ts`) rather than growing a second way to
			// start the same binary.
			name: 'portals',
			testMatch: 'portals/**/*.spec.ts',
			fullyParallel: false,
			workers: 1,
			dependencies: ['consent'],
			use: {
				...devices['Desktop Chrome'],
				channel: 'chromium',
				viewport: { width: 390, height: 844 },
				baseURL: `http://127.0.0.1:${bridgePort}`
			}
		},
		{
			// The session journeys (#111), on their own short-lived-token origin.
			//
			// One worker and not parallel, for the same reason as `dashboard`:
			// one Gateway, one device list, one login per bridge. These tests
			// revoke devices and complete logins, which is state the next one
			// would otherwise inherit.
			name: 'session',
			testMatch: 'session/**/*.spec.ts',
			fullyParallel: false,
			workers: 1,
			use: {
				...devices['Desktop Chrome'],
				channel: 'chromium',
				viewport: { width: 390, height: 844 },
				baseURL: `http://127.0.0.1:${sessionPort}`
			}
		}
	],

	// One server, run three times when a real stack is wanted: once in front of
	// the bootstrap Gateway, once in front of the bridge Gateway, once in front
	// of the session Gateway. Without the stack there is one static origin and
	// every spec that needs more skips.
	webServer: realStack
		? [
				{
					command: 'node tests/serve-like-gateway.mjs',
					url: `http://127.0.0.1:${port}/health`,
					// Never reuse when a real stack is wanted: a server already
					// listening is one with no Gateway behind it, and the
					// journey would fail obscurely instead of not running.
					reuseExistingServer: false,
					// Long enough for `docker compose up --wait` on a cold
					// Synapse and a cold `cargo build`; the default 60 s is not.
					timeout: 900_000,
					stdout: 'pipe',
					stderr: 'pipe'
				},
				{
					command: 'node tests/serve-like-gateway.mjs',
					url: `http://127.0.0.1:${bridgePort}/health`,
					env: {
						TWALK_TEST_PORT: String(bridgePort),
						TWALK_TEST_BRIDGE_STACK: '1'
					},
					reuseExistingServer: false,
					timeout: 900_000,
					stdout: 'pipe',
					stderr: 'pipe'
				},
				{
					command: 'node tests/serve-like-gateway.mjs',
					url: `http://127.0.0.1:${sessionPort}/health`,
					env: {
						TWALK_TEST_PORT: String(sessionPort),
						TWALK_TEST_SESSION_STACK: '1'
					},
					reuseExistingServer: false,
					timeout: 900_000,
					stdout: 'pipe',
					stderr: 'pipe'
				}
			]
		: {
				command: 'node tests/serve-like-gateway.mjs',
				url: `http://127.0.0.1:${port}/health`,
				reuseExistingServer: !process.env.CI,
				timeout: 60_000,
				stdout: 'pipe',
				stderr: 'pipe'
			}
});
