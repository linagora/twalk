// Screen 3 — the picker, against the real Gateway's bridge list.
//
// The grid is a join of two things the screen must keep apart: the networks
// v0.1 supports, and the bridges this deployment configured. The join is unit
// tested (`src/lib/networks/catalogue.test.ts`); what this proves is that the
// list really comes from `GET /api/bridges` and that the cards lead where the
// wireframe says.

import { expect, test } from '@playwright/test';

import { bridgeStack, NO_STACK, signIn } from './harness';

test.skip(bridgeStack() === null, NO_STACK);

test.beforeEach(async ({ context, request }) => {
	await signIn(context, request, 'the picker’s device');
});

test('shows the four v0.1 networks, Telegram and Discord as v0.2, and a skip link', async ({
	page
}) => {
	await page.goto('/networks');

	await expect(page.getByTestId('screen-networks')).toBeVisible();
	for (const network of ['whatsapp', 'signal', 'sms', 'matrix']) {
		await expect(page.getByTestId(`card-${network}`)).toBeVisible();
	}

	// The two v0.2 cards are shown so the user knows the roadmap, and are
	// never interactive.
	for (const network of ['telegram', 'discord']) {
		const card = page.getByTestId(`card-${network}`);
		await expect(card).toHaveAttribute('data-blocked', 'coming-soon');
		await expect(card.getByRole('link')).toHaveCount(0);
	}

	// The SMS card carries the discreet preview badge.
	await expect(page.getByTestId('card-sms')).toContainText(/Preview|Aperçu/);

	await expect(page.getByTestId('skip-networks')).toBeVisible();
});

test('the WhatsApp card leads to screen 3a', async ({ page }) => {
	await page.goto('/networks');
	await page.getByTestId('card-whatsapp').getByRole('link').click();
	await expect(page).toHaveURL(/\/networks\/whatsapp$/);
	await expect(page.getByTestId('screen-whatsapp')).toBeVisible();
});

test('greys the SMS preview on an iOS user agent, with its reason on screen', async ({
	browser,
	request
}) => {
	// The wireframe asks for a tooltip; a tooltip is invisible to a touch
	// screen and to a screen reader, so the reason is in the page as well.
	const context = await browser.newContext({
		viewport: { width: 390, height: 844 },
		userAgent:
			'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1'
	});
	await signIn(context, request, 'an iPhone');
	const page = await context.newPage();
	await page.goto('/networks');

	const sms = page.getByTestId('card-sms');
	await expect(sms).toHaveAttribute('data-blocked', 'ios');
	await expect(sms).toContainText(/Android/);
	await expect(sms.getByRole('link')).toHaveCount(0);

	// And only that card: WhatsApp and Signal are unaffected.
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-blocked', '');

	await context.close();
});

test('a signed-out browser is not shown a list it could not read', async ({ browser }) => {
	// No device cookie: the Gateway answers 401 and the screen says the list is
	// unknown rather than pretending nothing is configured.
	const context = await browser.newContext({ viewport: { width: 390, height: 844 } });
	const page = await context.newPage();
	await page.goto('/networks');

	await expect(page.getByTestId('bridges-unknown')).toBeVisible();
	// The cards stay tappable: each network's own screen handles it from there.
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-blocked', '');

	await context.close();
});
