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
			use: {
				...devices['Desktop Chrome'],
				channel: 'chromium',
				// The wireframes are mobile-first, 375–428 CSS px. Screen 1 is
				// asserted at the narrow end, where it has to work.
				viewport: { width: 390, height: 844 }
			}
		}
	],

	webServer: {
		command: 'node tests/serve-like-gateway.mjs',
		url: `http://127.0.0.1:${port}/health`,
		// Never reuse when a real stack is wanted: a server already listening
		// is one with no Gateway behind it, and the journey would fail
		// obscurely instead of not running.
		reuseExistingServer: !process.env.CI && !realStack,
		// Long enough for `docker compose up --wait` on a cold Synapse and a
		// cold `cargo build`; the default 60 s is not.
		timeout: realStack ? 900_000 : 60_000,
		stdout: 'pipe',
		stderr: 'pipe'
	}
});
