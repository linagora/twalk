// The capability gate, in a real browser with real APIs taken away.
//
// The unit tests in `src/lib/capabilities/report.test.ts` check what the
// report *decides*; these check that the browser is measured correctly and
// that the screen names the cause — including the one ADR 0014 singles out,
// iOS Lockdown Mode, whose signature is that WebAssembly keeps working while
// storage, service workers and tab locking do not.

import { expect, test, type Page } from '@playwright/test';

/** Empties a global before any of the app's own scripts run. */
async function withoutGlobals(page: Page, script: string) {
	await page.addInitScript(script);
}

test('lets a current browser on localhost through', async ({ page }) => {
	// `localhost` is a secure context, so nothing needs to be arranged.
	await page.goto('/');
	await expect(page.getByTestId('screen-welcome')).toBeVisible();
	await expect(page.getByTestId('capability-gate')).toHaveCount(0);
	await expect(page.getByTestId('capability-degraded')).toHaveCount(0);
});

test('names IndexedDB when storage refuses, and says why', async ({ page }) => {
	await withoutGlobals(
		page,
		`Object.defineProperty(window, 'indexedDB', {
			configurable: true,
			get() { throw new DOMException('blocked', 'SecurityError'); }
		});`
	);
	await page.goto('/');

	const gate = page.getByTestId('capability-gate');
	await expect(gate).toBeVisible();
	// Onboarding is not reachable behind the gate.
	await expect(page.getByTestId('screen-welcome')).toHaveCount(0);

	// The row names the capability and its state.
	const storage = page.locator('[data-capability="storage"]');
	await expect(storage).toHaveAttribute('data-state', 'blocked');
	await expect(storage).toContainText(/IndexedDB/);

	// Storage refusing on a secure origin with workers intact is a private
	// window or blocked site data, not Lockdown Mode.
	await expect(page.getByTestId('capability-cause')).toHaveAttribute(
		'data-cause',
		'storage-blocked'
	);
});

test('names WebAssembly when it is gone', async ({ page }) => {
	await withoutGlobals(
		page,
		`Object.defineProperty(window, 'WebAssembly', { configurable: true, value: undefined });`
	);
	await page.goto('/');

	await expect(page.getByTestId('capability-gate')).toBeVisible();
	await expect(page.locator('[data-capability="webassembly"]')).toHaveAttribute(
		'data-state',
		'missing'
	);
	await expect(page.getByTestId('capability-cause')).toHaveAttribute(
		'data-cause',
		'unsupported-browser'
	);
});

test('blames the insecure origin rather than listing what it took out', async ({ page }) => {
	// A page on plain http: the secure context is gone and storage, workers
	// and Web Crypto go with it. Four failures, one fix.
	await withoutGlobals(
		page,
		`Object.defineProperty(window, 'isSecureContext', { configurable: true, value: false });`
	);
	await page.goto('/');

	await expect(page.getByTestId('capability-gate')).toBeVisible();
	await expect(page.getByTestId('capability-cause')).toHaveAttribute(
		'data-cause',
		'insecure-context'
	);
	await expect(page.getByTestId('capability-cause')).toContainText(/HTTPS/i);
});

test('recognises iOS Lockdown Mode by what it leaves running', async ({ page }) => {
	// ADR 0014's exact case: IndexedDB, service workers and Web Locks off,
	// WebAssembly still on. The fix is one iOS setting, and the gate has to
	// say so instead of listing three missing APIs.
	await withoutGlobals(
		page,
		`Object.defineProperty(window, 'indexedDB', { configurable: true, value: undefined });
		 Object.defineProperty(navigator, 'serviceWorker', { configurable: true, value: undefined });
		 Object.defineProperty(navigator, 'locks', { configurable: true, value: undefined });`
	);
	await page.goto('/');

	await expect(page.getByTestId('capability-gate')).toBeVisible();
	await expect(page.getByTestId('capability-cause')).toHaveAttribute('data-cause', 'lockdown-mode');
	await expect(page.getByTestId('capability-cause')).toContainText(/Lockdown Mode/i);
	// WebAssembly is reported as present, which is the whole point.
	await expect(page.locator('[data-capability="webassembly"]')).toHaveAttribute(
		'data-state',
		'available'
	);
});

test('does not block when only installability is missing', async ({ page }) => {
	// Everything onboarding needs is there; the home-screen install and the
	// two-tab lock are not. The user goes through, and is told.
	await withoutGlobals(
		page,
		`Object.defineProperty(navigator, 'serviceWorker', { configurable: true, value: undefined });
		 Object.defineProperty(navigator, 'locks', { configurable: true, value: undefined });`
	);
	await page.goto('/');

	await expect(page.getByTestId('screen-welcome')).toBeVisible();
	await expect(page.getByTestId('capability-gate')).toHaveCount(0);
	await expect(page.getByTestId('capability-degraded')).toBeVisible();
});

test('diagnostics stays reachable from behind the gate', async ({ page }) => {
	// It is where the gate sends the user, so it must render when nothing
	// else can — and it must report the same cause the gate did.
	await withoutGlobals(
		page,
		`Object.defineProperty(window, 'indexedDB', { configurable: true, value: undefined });
		 Object.defineProperty(navigator, 'serviceWorker', { configurable: true, value: undefined });
		 Object.defineProperty(navigator, 'locks', { configurable: true, value: undefined });`
	);
	await page.goto('/');
	await expect(page.getByTestId('capability-gate')).toBeVisible();

	await page.getByRole('link', { name: /diagnostics/i }).click();
	await expect(page.getByTestId('screen-diagnostics')).toBeVisible();
	await expect(page.getByTestId('capability-cause-id')).toHaveAttribute(
		'data-cause',
		'lockdown-mode'
	);
	await expect(page.getByTestId('diagnostics-text')).toContainText('storage: missing (required)');
});

test('copies diagnostics to the clipboard instead of sending them anywhere', async ({
	page,
	context
}) => {
	await context.grantPermissions(['clipboard-read', 'clipboard-write']);
	await page.goto('/diagnostics');
	await expect(page.getByTestId('screen-diagnostics')).toBeVisible();

	await page.getByTestId('copy-diagnostics').click();
	const copied = await page.evaluate(() => navigator.clipboard.readText());
	expect(copied).toContain('Twalk Companion diagnostics');
	expect(copied).toContain('built for gateway: 0.1.0');
	expect(copied).toContain('No telemetry is collected');
});
