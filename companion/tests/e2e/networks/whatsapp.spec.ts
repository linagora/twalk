// Screen 3a — the WhatsApp QR login, against the real Gateway and a stub
// bridge. The anchor journey of v0.1: if this works, the user believes in
// Twalk.
//
// The bridge is stubbed for the reason spec #47 gives — a real
// mautrix-whatsapp needs a live account and a human with a phone — but nothing
// else is: the Gateway is the binary it ships as, it holds the bridge's
// blocking step itself, and the browser polls. What the code on screen is drawn
// from is the raw payload the stub handed over, through the Gateway, into the
// page.

import { expect, test } from '@playwright/test';

import {
	bridgeStack,
	clearLogin,
	drawnCode,
	NO_STACK,
	signIn,
	StubBridge,
	WHATSAPP_BRIDGE
} from './harness';

test.skip(bridgeStack() === null, NO_STACK);

test.describe.configure({ mode: 'serial' });

test.beforeEach(async ({ context, request }) => {
	const token = await signIn(context, request, 'the scanning device');
	// The Gateway is one process for the whole suite: a login left in flight
	// would meet the next test as the concurrent-login refusal.
	await clearLogin(request, token, WHATSAPP_BRIDGE);
	await new StubBridge(request, WHATSAPP_BRIDGE).reset();
});

test('a whole QR login, from the disclosure to the connected network', async ({ page, request }) => {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');

	// *Disclosure shown*: the code is hidden until the ban-risk notice is
	// confirmed. The wireframe is explicit that this comes first.
	await expect(page.getByTestId('disclosure')).toBeVisible();
	await expect(page.getByTestId('qr-code')).toHaveCount(0);
	await expect(page.getByTestId('disclosure')).toContainText(/suspend|bann/i);

	await page.getByTestId('accept-disclosure').click();

	// *Waiting for scan*: a code drawn in the browser, with the text
	// alternative the accessibility section requires and a countdown.
	const code = page.getByTestId('qr-code');
	await expect(code).toBeVisible();
	await expect(code).toHaveAttribute('aria-label', /Linked devices|Appareils connectés/);
	await expect(page.getByTestId('countdown')).toBeVisible();

	// The Gateway is the one holding the bridge's blocking step: the browser
	// has made no long-lived request, and the stub has exactly one caller
	// sitting in `display_and_wait`.
	await expect
		.poll(async () => (await bridge.stats()).blocking_arrivals, {
			message: 'the Gateway sits down in the blocking step'
		})
		.toBeGreaterThanOrEqual(1);
	expect((await bridge.stats()).starts[0]?.flow_id).toBe('qr');

	// *Scanned*: the bridge completes the login, and the next poll picks it up.
	await bridge.releaseCompletion('33612345678');
	await expect(page.getByTestId('login-complete')).toBeVisible();
	await expect(page.getByTestId('login-complete')).toContainText(/WhatsApp/);

	// And the Gateway now reports the bridge as connected, which is what
	// screen 3 marks with a check.
	await page.getByRole('link', { name: /Continue|Continuer/ }).click();
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connected', 'yes');
});

test('a refresh mid-flow redraws the code before it expires', async ({ page, request }) => {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();

	const first = await drawnCode(page);

	// A real bridge refreshes the code every ~20 seconds while nobody has
	// scanned. The Gateway bumps `generation`; the browser redraws on that,
	// not on a clock.
	await bridge.releaseRefreshedQr('2@a-second-whatsapp-code-payload');
	await expect.poll(async () => drawnCode(page)).not.toBe(first);

	// Still the same login: a refresh is a new step, not a new process.
	expect((await bridge.stats()).starts).toHaveLength(1);

	// And it still completes, on the refreshed code.
	await bridge.releaseCompletion('33612345678');
	await expect(page.getByTestId('login-complete')).toBeVisible();
});

test('a cancelled login says so, and leaves nothing running on the bridge', async ({
	page,
	request
}) => {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();

	await page.getByTestId('cancel-login').click();

	await expect(page.getByTestId('login-cancelled')).toBeVisible();
	await expect
		.poll(async () => (await bridge.stats()).cancelled.length)
		.toBeGreaterThanOrEqual(1);

	// The way out is on screen, and it starts a fresh login.
	await page.getByTestId('restart').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();
	expect((await bridge.stats()).starts.length).toBeGreaterThanOrEqual(2);
});

test('a second device is refused, and the refusal is a screen', async ({
	browser,
	page,
	request
}) => {
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();

	// A second device of the same owner — one login per bridge in v0.1.
	const second = await browser.newContext({ viewport: { width: 390, height: 844 } });
	await signIn(second, request, 'the laptop in the kitchen');
	const other = await second.newPage();
	// The disclosure is per device, so this one sees it too.
	await other.goto('/networks/whatsapp');
	await other.getByTestId('accept-disclosure').click();

	const refusal = other.getByTestId('login-in-flight');
	await expect(refusal).toBeVisible();
	// The wireframe's requirement: which device, and when.
	await expect(refusal).toContainText('the scanning device');
	await expect(refusal).toContainText(/20\d\d-/);
	// Not a raw error document.
	await expect(refusal).not.toContainText('login_in_flight');

	// And the way out the acceptance criteria ask for: take it over.
	await other.getByTestId('take-over').click();
	await expect(other.getByTestId('qr-code')).toBeVisible();

	await second.close();
});

test('a login lost to a bridge restart says so rather than hanging', async ({ page, request }) => {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();

	await bridge.restart();

	const failed = page.getByTestId('login-failed');
	await expect(failed).toBeVisible();
	await expect(failed).toHaveAttribute('data-error-code', 'login_lost');
	await expect(failed).toContainText(/redémarr|restart/i);
});
