// Screen 3c — SMS through Google Messages, the v0.1 preview path.
//
// The bridge is the stub, answering the cookies flow the way mautrix-gmessages
// does: a `cookies` step, then the emoji pairing step, then the completion. The
// assertions the wireframe cares about are the honesty ones — the preview
// framing before anything else, the two browser settings named, and the cookies
// never written to this browser.

import { expect, test } from '@playwright/test';

import { bridgeStack, clearLogin, NO_STACK, signIn, SMS_BRIDGE, StubBridge } from './harness';

test.skip(bridgeStack() === null, NO_STACK);
test.describe.configure({ mode: 'serial' });

/** A plausible jar: the seven Google session cookies, in header spelling. */
const NAMES = ['SID', 'HSID', 'SSID', 'APISID', 'SAPISID', '__Secure-1PSID', '__Secure-1PSIDTS'];
const PASTE = NAMES.map((name) => `${name}=value-for-${name}`).join('; ');

test.beforeEach(async ({ context, request }) => {
	const token = await signIn(context, request, 'the SMS device');
	await clearLogin(request, token, SMS_BRIDGE);
	await new StubBridge(request, SMS_BRIDGE).reset();
});

test('the preview framing comes before the login, and says all four things', async ({ page }) => {
	await page.goto('/networks/sms');

	const disclosure = page.getByTestId('disclosure');
	await expect(disclosure).toBeVisible();
	// The milestone's four: Google Messages Web, a Google account, not for
	// iOS-only users, replaced in v0.2.
	await expect(disclosure).toContainText(/Google Messages Web/);
	await expect(disclosure).toContainText(/Google account|compte Google/);
	await expect(disclosure).toContainText(/v0\.2/);
	await expect(page.getByTestId('screen-sms')).toContainText(/Preview|Aperçu/);

	// Nothing to paste until the notice is confirmed.
	await expect(page.getByTestId('cookie-paste')).toHaveCount(0);

	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('screen-sms')).toContainText(/iPhone alone|iPhone seul/);
});

test('a whole cookie login: the cookies, the emoji, the connection', async ({ page, request }) => {
	const bridge = new StubBridge(request, SMS_BRIDGE);
	await page.goto('/networks/sms');
	await page.getByTestId('accept-disclosure').click();

	// The cookie names come from the bridge's own step, not from a list this
	// app made up.
	const names = page.getByTestId('cookie-names');
	await expect(names).toBeVisible();
	await expect(names).toContainText('SID');

	// #57's four requirements, all of them, after the migration onto the shared
	// renderer (ADR 0030): which cookies (above), where to get them, why a
	// private window, and that Device Bound Session Credentials must be off.
	// Asserted as four because losing any one of them fails that ticket, and a
	// migration is exactly where prose goes missing without anyone noticing.
	const screen = page.getByTestId('screen-sms');
	await expect(
		screen.getByRole('link', { name: /messages\.google\.com/ }).first()
	).toBeVisible();
	await expect(screen).toContainText(/private window|fenêtre de navigation privée/);
	await expect(screen).toContainText(/Device Bound Session Credentials/);
	// And the sentence that lets someone decide not to: what holding the whole
	// jar means.
	await expect(screen).toContainText(/act as you on Google Messages Web|agir en votre nom/);

	// A paste that is not cookies is named as such, and nothing is sent.
	await page.getByTestId('cookie-paste').fill('I could not find them');
	await page.getByTestId('submit-step').click();
	await expect(page.getByRole('alert')).toContainText(/could not be read|Impossible d’y lire|Impossible d'y lire/);

	await page.getByTestId('cookie-paste').fill(PASTE);
	await page.getByTestId('submit-step').click();

	// The emoji pairing step: the same blocking step the QR screens poll, with
	// an emoji instead of a code, and nothing to submit.
	const emoji = page.getByTestId('sms-emoji');
	await expect(emoji).toBeVisible();
	await expect(emoji).toContainText('🐢');

	// The cookies reached the bridge, unchanged and whole.
	const stats = await bridge.stats();
	const relayed = stats.submits?.find((submit) => submit.step_type === 'cookies');
	expect(relayed, JSON.stringify(stats.submits)).toBeDefined();
	expect(relayed?.body?.cookies?.['SID']).toBe('value-for-SID');
	expect(Object.keys(relayed?.body?.cookies ?? {})).toHaveLength(NAMES.length);

	// And they are in no browser store, nor left on screen.
	await expect(page.getByTestId('cookie-paste')).toHaveCount(0);
	const stored = await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }));
	expect(stored).not.toContain('value-for-SID');

	await bridge.releaseCompletion('gmessages-login');
	await expect(page.getByTestId('login-complete')).toBeVisible();
	await expect(page.getByTestId('login-complete')).toContainText(/v0\.2/);
});

test('an iPhone is told plainly, and given a way out', async ({ browser, request }) => {
	const context = await browser.newContext({
		viewport: { width: 390, height: 844 },
		userAgent:
			'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1'
	});
	await signIn(context, request, 'an iPhone');
	const page = await context.newPage();
	await page.goto('/networks/sms');

	const explained = page.getByTestId('sms-ios');
	await expect(explained).toBeVisible();
	await expect(explained).toContainText(/Android/);
	await expect(page.getByTestId('cookie-paste')).toHaveCount(0);
	await expect(page.getByTestId('disclosure')).toHaveCount(0);
	await page.getByRole('link', { name: /Skip SMS|Passer les SMS/ }).click();
	await expect(page).toHaveURL(/\/networks$/);

	await context.close();
});
