// Screen 3 — the picker, against the real Gateway's bridge list.
//
// The grid is a join of two things the screen must keep apart: the networks
// v0.1 supports, and the bridges this deployment configured. The join is unit
// tested (`src/lib/networks/catalogue.test.ts`); what this proves is that the
// list really comes from `GET /api/bridges` and that the cards lead where the
// wireframe says.

import { expect, test } from '@playwright/test';

import {
	bridgeStack,
	NO_STACK,
	signIn,
	SIGNAL_BRIDGE,
	SMS_BRIDGE,
	StubBridge,
	WHATSAPP_BRIDGE
} from './harness';

test.skip(bridgeStack() === null, NO_STACK);

test.beforeEach(async ({ context, request }) => {
	await signIn(context, request, 'the picker’s device');
	// What a card says now depends on what its bridge holds (#108), and every
	// spec in this project shares one stub. So this one states the world it is
	// asserting about instead of inheriting whatever ran before it: no bridge
	// holds a login, so no card is connected.
	for (const bridgeId of [WHATSAPP_BRIDGE, SIGNAL_BRIDGE, SMS_BRIDGE]) {
		await new StubBridge(request, bridgeId).reset();
	}
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

test('an unconnected WhatsApp card leads to screen 3a', async ({ page }) => {
	// Nothing is linked, so the card offers *Continue* and leads to the
	// login. A card with a link on it leads to the management screen instead;
	// that is `manage.spec.ts`.
	await page.goto('/networks');
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-linked', 'no');
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

test('with nothing connected, it is step 1 of onboarding', async ({ page }) => {
	// The `beforeEach` above reset every stub bridge, so no bridge holds a
	// login and nothing is connected. This is the journey the current copy was
	// written for, and it stays exactly as it was.
	await page.goto('/networks');
	const screen = page.getByTestId('screen-networks');
	await expect(screen).toHaveAttribute('data-mode', 'first');
	await expect(page.getByRole('heading', { level: 1 })).toHaveText(
		/first network|premier réseau/i
	);
	await expect(page.getByTestId('networks-step')).toBeVisible();
	await expect(page.getByTestId('skip-networks')).toBeVisible();
});

test('with one connected, it is adding a network — no "first", no step counter', async ({
	page,
	request
}) => {
	// Observed live with WhatsApp connected for two hours and Signal for two
	// minutes, on a screen that said "Connect your first network — step 1 of
	// 3" (#120). The branch is taken from the deployment's own state: a bridge
	// that holds a live session, which is what `connection.ts` reads.
	const whatsapp = new StubBridge(request, WHATSAPP_BRIDGE);
	await whatsapp.addExistingLogin('a-live-session', '+33660469852', 'CONNECTED');

	await page.goto('/networks');
	const screen = page.getByTestId('screen-networks');
	await expect(screen).toHaveAttribute('data-mode', 'add');

	const heading = page.getByRole('heading', { level: 1 });
	await expect(heading).toHaveText(/add a network|ajouter un réseau/i);
	await expect(heading).not.toHaveText(/first|premier/i);

	// No step counter, and no offer to skip a journey this user is not in.
	await expect(page.getByTestId('networks-step')).toHaveCount(0);
	await expect(page.getByTestId('skip-networks')).toHaveCount(0);
	await expect(page.getByTestId('back-to-dashboard')).toBeVisible();

	// And the connected network is presented as connected rather than as a
	// choice: its card carries the badge and leads to its management screen.
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connected', 'yes');
	await expect(page.getByTestId('state-whatsapp')).toBeVisible();
	await expect(page.getByTestId('manage-whatsapp')).toBeVisible();

	await whatsapp.reset();
});

test('a Gateway that did not answer claims neither journey', async ({ browser }) => {
	// "I could not ask" is not "nothing is connected". The heading that can be
	// false is the one that is withheld.
	const context = await browser.newContext({ viewport: { width: 390, height: 844 } });
	const page = await context.newPage();
	await page.goto('/networks');

	await expect(page.getByTestId('bridges-unknown')).toBeVisible();
	await expect(page.getByTestId('screen-networks')).toHaveAttribute('data-mode', 'unknown');
	await expect(page.getByTestId('networks-step')).toHaveCount(0);
	await expect(page.getByRole('heading', { level: 1 })).not.toHaveText(/first|premier/i);

	await context.close();
});
