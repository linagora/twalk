// Screen 3b — Signal, paired as a linked (secondary) device.
//
// The same lifecycle as screen 3a, and deliberately the same component: what
// differs is the words. So this spec asserts the two differences that matter —
// no disclosure card, and the history limitation stated — plus that the code
// really is drawn and really does complete.

import { expect, test } from '@playwright/test';

import { bridgeStack, clearLogin, NO_STACK, signIn, SIGNAL_BRIDGE, StubBridge } from './harness';

test.skip(bridgeStack() === null, NO_STACK);

test.describe.configure({ mode: 'serial' });

test.beforeEach(async ({ context, request }) => {
	const token = await signIn(context, request, 'the pairing device');
	await clearLogin(request, token, SIGNAL_BRIDGE);
	await new StubBridge(request, SIGNAL_BRIDGE).reset();
});

test('pairs without a disclosure card, and says what history it will not see', async ({
	page,
	request
}) => {
	const bridge = new StubBridge(request, SIGNAL_BRIDGE);
	await page.goto('/networks/signal');

	// Signal officially supports linked devices: there is no ban risk to
	// disclose, so the code is on screen immediately.
	await expect(page.getByTestId('disclosure')).toHaveCount(0);

	const code = page.getByTestId('qr-code');
	await expect(code).toBeVisible();
	await expect(code).toHaveAttribute('aria-label', /Linked devices|Appareils liés/);

	// The wireframe's calm chip: the limitation the user must know about.
	await expect(page.getByTestId('screen-signal')).toContainText(
		/not synchronised|pas synchronisés/
	);

	await bridge.releaseCompletion('signal-device-1');
	await expect(page.getByTestId('login-complete')).toBeVisible();
	await expect(page.getByTestId('login-complete')).toContainText(/Signal/);
});

test('a refreshed code is redrawn here too', async ({ page, request }) => {
	const bridge = new StubBridge(request, SIGNAL_BRIDGE);
	await page.goto('/networks/signal');
	const first = await page.getByTestId('qr-code').locator('path').getAttribute('d');

	await bridge.releaseRefreshedQr('sgnl://linkdevice?uuid=second&pub_key=second');
	await expect
		.poll(async () => page.getByTestId('qr-code').locator('path').getAttribute('d'))
		.not.toBe(first);
});
