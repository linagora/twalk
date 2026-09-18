// Screen 5 — the home screen: what it shows, what it refuses to show, and the
// two things a user does from it.
//
// The privacy assertion is not decoration. The wireframe's feed was "the last
// 10 events on the bus (received messages…)" with who sent them; the design
// review replaced it with operational events and a count (#74). This spec
// checks the rendered page for a correspondent's identity, because that is the
// form the regression would take: a later ticket adds a row, the row carries a
// subject id, and nobody notices until the screen is open on a train.

import { expect, test } from '@playwright/test';

import {
	bridgeStack,
	clearLogin,
	connectWhatsApp,
	expireWhatsAppSession,
	NO_STACK,
	sessionStatus,
	signIn,
	signInExtraDevice,
	StubBridge,
	WHATSAPP_BRIDGE
} from './harness';

const stack = bridgeStack();

test.skip(stack === null, NO_STACK);

test.describe.configure({ mode: 'serial' });

let deviceToken = '';

test.beforeEach(async ({ context, request }) => {
	deviceToken = await signIn(context, request, 'the c69 dashboard device');
	await clearLogin(request, deviceToken, WHATSAPP_BRIDGE);
	await new StubBridge(request, WHATSAPP_BRIDGE).reset();
});

test('a bridge state change surfaces in the dashboard, with the amber banner and a way back', async ({
	page,
	request
}) => {
	await connectWhatsApp(page, request);

	await page.goto('/dashboard');
	await expect(page.getByTestId('bridge-whatsapp')).toHaveAttribute('data-state', 'connected');
	await expect(page.getByTestId('expired-whatsapp')).toHaveCount(0);

	// The bridge loses the session — mautrix answers `410` on the held step,
	// which the Gateway records as `login_expired`.
	await expireWhatsAppSession(request, deviceToken);

	await page.goto('/dashboard');
	await expect(page.getByTestId('bridge-whatsapp')).toHaveAttribute('data-state', 'expired');

	// The wireframe's amber state, with the action it names.
	const banner = page.getByTestId('expired-whatsapp');
	await expect(banner).toBeVisible();
	await expect(banner).toContainText(/expired|expiré/i);
	await expect(page.getByTestId('reconnect')).toHaveAttribute('href', '/networks/whatsapp');

	// And the overall indicator stops being calm.
	await expect(page.getByTestId('screen-dashboard')).toHaveAttribute('data-health', 'attention');
});

test('the activity feed carries operational events and never a correspondent', async ({
	page,
	request
}) => {
	await connectWhatsApp(page, request);
	await page.goto('/dashboard');

	const feed = page.getByTestId('activity');
	await expect(feed).toBeVisible();
	// Something operational is in it: the bridge that was just connected.
	await expect(feed).toContainText(/WhatsApp/);

	// The message slot exists and says it is not counted, rather than showing
	// a zero the user would read as "nothing arrived".
	await expect(page.getByTestId('messages-carried')).toContainText(
		/not counted|non comptés/i
	);

	// No Matrix user ID anywhere on the home screen. The deployment's own
	// owner is a Matrix ID too, and the dashboard does not print it either:
	// the whole screen is checked, not just the feed.
	const rendered = (await page.getByTestId('screen-dashboard').innerText()).replace(/\s+/gu, ' ');
	expect(rendered).not.toMatch(/@[a-z0-9._=\-/]+:[a-z0-9.\-]+/iu);

	// And the screen says why, so the absence reads as a decision.
	await expect(page.getByTestId('feed-privacy')).toContainText(
		/who writes to you|qui vous écrit/i
	);
});

test('revoking a device from the dashboard refuses its session on the next request', async ({
	page,
	request
}) => {
	const extra = await signInExtraDevice(request, 'the phone I lost');

	// It works before the revocation, or the assertion after it would prove
	// nothing.
	expect(await sessionStatus(request, extra.token)).toBe(200);

	await page.goto('/dashboard');
	const row = page.getByTestId(`device-${extra.id}`);
	await expect(row).toBeVisible();
	await expect(row).toContainText(extra.name);
	await expect(row).toHaveAttribute('data-revoked', 'no');

	await page.getByTestId(`revoke-${extra.id}`).click();

	// The row stays, dated, so "I revoked that phone" remains visible — which
	// is what the Gateway's device list is for.
	await expect(row).toHaveAttribute('data-revoked', 'yes');

	// The token stops working on its very next request. Nothing is cached.
	expect(await sessionStatus(request, extra.token)).toBe(401);
});
