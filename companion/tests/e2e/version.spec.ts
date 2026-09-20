// The version handshake and the service worker, which are one subject: the
// worker is what lets an app shell outlive a Gateway upgrade, and the
// handshake is what notices.

import { expect, test } from '@playwright/test';

import { cachedPaths, registrationCount, waitForCachedShell } from './service-worker';

test('a matching Gateway version passes in silence', async ({ page }) => {
	// The test server reports 0.1.0, the version in
	// companion-gateway/openapi.yaml that the client generator baked in.
	await page.goto('/');
	await expect(page.getByTestId('screen-welcome')).toBeVisible();
	await expect(page.getByTestId('version-banner')).toHaveCount(0);
});

test('a moved Gateway version forces a reload, then says so', async ({ page }) => {
	let healthRequests = 0;
	await page.route('**/health', async (route) => {
		healthRequests += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ status: 'ok', version: '9.9.9', revision: 'newer' })
		});
	});

	await page.goto('/');

	// First boot sees the mismatch and reloads. The second boot sees the same
	// mismatch, finds its own marker, and shows the banner instead of looping.
	const banner = page.getByTestId('version-banner');
	await expect(banner).toBeVisible();
	await expect(banner).toHaveAttribute('data-kind', 'mismatch');
	await expect(banner).toContainText('0.1.0');
	await expect(banner).toContainText('9.9.9');

	// Exactly one reload: the handshake ran twice, not forever.
	expect(healthRequests).toBe(2);

	// And the app is still usable while it says so.
	await expect(page.getByTestId('screen-welcome')).toBeVisible();
});

test('a mismatch purges the cached shell before reloading', async ({ page }) => {
	// Register the worker with a matching version first, so there is a cache
	// and a registration to purge.
	await page.goto('/');
	await waitForCachedShell(page);

	await page.route('**/health', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ status: 'ok', version: '9.9.9', revision: 'newer' })
		})
	);
	await page.reload();

	await expect(page.getByTestId('version-banner')).toBeVisible();
	// Both halves matter: a registered worker would answer the very
	// navigation the reload triggers, out of a cache we had just emptied.
	expect(await cachedPaths(page)).toEqual([]);
	expect(await registrationCount(page)).toBe(0);
});

test('a Gateway that ships another build of the Companion is a stale shell: one reload, then the banner', async ({
	page
}) => {
	// #222, the case it was filed on: the Companion redeployed, the Gateway
	// unchanged, and an open tab still running the previous build — five
	// submissions of a fixed form went out in the old shape while the server
	// reported itself current. The Gateway now names the build it ships
	// (`companion_build`, off the export's `_app/version.json`) and this page
	// knows its own, so the difference is a reload rather than a mystery.
	//
	// Register the worker with a matching answer first, so there is a cache
	// and a registration for the reload to purge: a worker left registered
	// would answer the very navigation the reload triggers.
	await page.goto('/diagnostics');
	await waitForCachedShell(page);
	const running = (await page.getByTestId('app-build').textContent())?.trim() ?? '';
	expect(running).not.toBe('');

	let healthRequests = 0;
	await page.route('**/health', async (route) => {
		healthRequests += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({
				status: 'ok',
				version: '0.1.0',
				revision: 'test',
				companion_build: 'a-build-this-page-is-not'
			})
		});
	});
	await page.reload();

	// First boot sees the stale shell and reloads; the second finds its marker
	// and says so instead of looping. Same shape as a moved Gateway, because
	// it is the same remedy.
	const banner = page.getByTestId('version-banner');
	await expect(banner).toBeVisible();
	await expect(banner).toHaveAttribute('data-kind', 'stale-shell');
	const builds = page.getByTestId('stale-shell-builds');
	await expect(builds).toHaveAttribute('data-shipped', 'a-build-this-page-is-not');
	await expect(builds).toHaveAttribute('data-running', running);
	expect(healthRequests).toBe(2);
	// The reload took the cached shell and the worker with it.
	expect(await cachedPaths(page)).toEqual([]);
	expect(await registrationCount(page)).toBe(0);
	// And the page is still usable while it says so — the diagnostics page
	// names both builds, so a mismatch is read rather than inferred.
	await expect(page.getByTestId('shipped-build')).toHaveText('a-build-this-page-is-not');
	await expect(page.getByTestId('handshake-kind')).toHaveAttribute('data-kind', 'stale-shell');
});

test('the build the Gateway ships and the build this page runs are one and the same on a fresh load', async ({
	page
}) => {
	// The test server reads `_app/version.json` off the export it serves, as
	// the Gateway does, so on a healthy origin the two agree and nothing is
	// said — the common case is silence.
	await page.goto('/diagnostics');
	const running = (await page.getByTestId('app-build').textContent())?.trim();
	await expect(page.getByTestId('shipped-build')).toHaveText(running ?? '');
	await expect(page.getByTestId('handshake-kind')).toHaveAttribute('data-kind', 'match');
	await expect(page.getByTestId('version-banner')).toHaveCount(0);
});

test('an unreachable Gateway is not a mismatch', async ({ page }) => {
	// A version-less answer must never trigger a reload: it would loop.
	await page.route('**/health', (route) => route.abort('connectionrefused'));
	await page.goto('/');

	await expect(page.getByTestId('version-banner')).toHaveAttribute('data-kind', 'unreachable');
	// The app still loads — the wireframes' screen 1 does not need the
	// Gateway to render.
	await expect(page.getByTestId('screen-welcome')).toBeVisible();
});

test('installability: a manifest, icons, and a worker caching fingerprints only', async ({
	page
}) => {
	await page.goto('/');

	const manifest = await page.evaluate(async () => {
		const href = document.querySelector('link[rel="manifest"]')?.getAttribute('href');
		if (href === null || href === undefined) {
			return null;
		}
		const response = await fetch(href);
		return {
			type: response.headers.get('content-type'),
			body: (await response.json()) as {
				name: string;
				display: string;
				icons: { sizes: string; purpose?: string }[];
			}
		};
	});
	expect(manifest?.type).toBe('application/manifest+json');
	expect(manifest?.body).toMatchObject({ name: 'Twalk Companion', display: 'standalone' });
	// Chrome's installability floor: a 192 px and a 512 px icon, plus a
	// maskable one so a launcher can crop it to its own shape.
	const icons = manifest?.body.icons ?? [];
	expect(icons.map((icon) => icon.sizes)).toContain('192x192');
	expect(icons.map((icon) => icon.sizes)).toContain('512x512');
	expect(icons.some((icon) => icon.purpose === 'maskable')).toBe(true);

	// The worker registers itself only after the handshake matched, and its
	// install fills the cache — with the fingerprinted build alone. Anything
	// whose name outlives a build is how a stale shell survives an upgrade.
	const cached = await waitForCachedShell(page);
	expect(cached.every((path) => path.startsWith('/_app/immutable/'))).toBe(true);
	expect(cached).not.toContain('/');
	expect(cached).not.toContain('/200.html');
	expect(cached).not.toContain('/manifest.webmanifest');
});

test('the worker never caches the Gateway’s API, and queues no writes', async ({ page }) => {
	// Spec #65: no offline writes. A queued consent decision could be
	// replayed hours later against changed state.
	await page.goto('/');
	await waitForCachedShell(page);

	const status = await page.evaluate(async () => (await fetch('/api/session')).status);
	// Straight through to the origin, which refuses it. A cached 200 here
	// would be the app believing a session it does not have.
	expect(status).toBe(401);

	expect(await cachedPaths(page)).not.toContain('/api/session');
	expect(await cachedPaths(page)).not.toContain('/health');
});
