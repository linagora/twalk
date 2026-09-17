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
		reuseExistingServer: !process.env.CI,
		stdout: 'pipe',
		stderr: 'pipe'
	}
});
