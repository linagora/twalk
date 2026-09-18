// Ticket #108 — the two journeys the ticket names, against the real Gateway.
//
// What went wrong in production, in one sentence: the network picker read its
// connected badge from `bridge.login.state === 'complete'` — the state of a
// login *process* living in the Gateway's memory — so a WhatsApp account that
// the bridge reported as `logins: 1, CONNECTED` throughout showed no badge at
// all, and *Manage* on it started a fresh QR flow the Gateway then refused
// with a 409. The owner concluded their link was broken and was about to
// re-pair a working connection.
//
// So the two journeys are:
//
//   1. connected → manage → disconnect → not connected → connect again;
//   2. a login in flight must not make a connected network look disconnected.
//
// Both are driven through the browser, against the Gateway binary, with the
// stub bridge on the far side answering what a real mautrix bridge answers.
// The link the journeys assert on is the bridge's — it is created and removed
// through the bridge's own provisioning API, never faked in the Companion.

import { expect, test } from '@playwright/test';

import {
	bridgeStack,
	clearLogin,
	NO_STACK,
	signIn,
	StubBridge,
	WHATSAPP_BRIDGE
} from './harness';

test.skip(bridgeStack() === null, NO_STACK);

test.describe.configure({ mode: 'serial' });

/** The account the reference deployment's incident was about. */
const ACCOUNT = '33660469852';
const ACCOUNT_NAME = '+33660469852';

test.beforeEach(async ({ context, request }) => {
	const token = await signIn(context, request, 'the managing device');
	// The Gateway is one process for the whole suite: a login left in flight
	// would meet the next test as the concurrent-login refusal.
	await clearLogin(request, token, WHATSAPP_BRIDGE);
	await new StubBridge(request, WHATSAPP_BRIDGE).reset();
});

test.afterEach(async ({ context, request }) => {
	// These journeys deliberately leave a *link* on the bridge, which is now a
	// thing the picker reports. Clear it, so the next spec starts from the
	// world it describes rather than this one's.
	const token = await signIn(context, request, 'the tidying device');
	await clearLogin(request, token, WHATSAPP_BRIDGE);
	await new StubBridge(request, WHATSAPP_BRIDGE).reset();
});

test('connected, managed, disconnected, and connected again', async ({ page, request }) => {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await bridge.addExistingLogin(ACCOUNT, ACCOUNT_NAME);

	// --- Connected ---------------------------------------------------------
	// The bridge holds a live login and no login process exists anywhere, so
	// this is exactly the state that used to show nothing at all.
	await page.goto('/networks');
	const card = page.getByTestId('card-whatsapp');
	await expect(card).toHaveAttribute('data-connection', 'connected');
	await expect(card).toHaveAttribute('data-connected', 'yes');
	await expect(page.getByTestId('state-whatsapp')).toContainText(/Connected|Connecté/);
	// And it names the account, which is the user's actual question.
	await expect(page.getByTestId('account-whatsapp')).toHaveText(ACCOUNT_NAME);

	// --- Manage ------------------------------------------------------------
	await page.getByTestId('manage-whatsapp').click();
	await expect(page).toHaveURL(/\/networks\/whatsapp\/manage$/);
	await expect(page.getByTestId('screen-manage-whatsapp')).toBeVisible();

	// It opened a management screen, not a login: no code is drawn, and the
	// Gateway was never asked to start one.
	await expect(page.getByTestId('qr-code')).toHaveCount(0);
	expect((await bridge.stats()).starts).toHaveLength(0);

	// The three facts the ticket asks for, and the two actions.
	await expect(page.getByTestId('manage-account')).toContainText(ACCOUNT_NAME);
	await expect(page.getByTestId('manage-since')).not.toBeEmpty();
	await expect(page.getByTestId('manage-state')).toContainText(/Connected|Connecté/);
	await expect(page.getByTestId('relink')).toBeVisible();
	await expect(page.getByTestId('disconnect')).toBeVisible();

	// --- Disconnect --------------------------------------------------------
	await page.getByTestId('disconnect').click();

	// The confirmation says what stops and what does not. A user who thinks
	// disconnecting deletes their conversations will not disconnect a session
	// they should.
	const confirm = page.getByTestId('disconnect-confirm');
	await expect(confirm).toBeVisible();
	await expect(page.getByTestId('disconnect-stops')).toContainText(/Sensor/);
	await expect(page.getByTestId('disconnect-keeps')).toContainText(
		/rooms already created|salons déjà créés/i
	);
	await expect(page.getByTestId('disconnect-keeps')).toContainText(
		/Nothing is deleted|Rien n'est supprimé/i
	);

	await page.getByTestId('disconnect-yes').click();

	// The bridge really dropped the login: this is the bridge's own record,
	// not the Companion's opinion of it.
	await expect
		.poll(async () => (await bridge.stats()).logged_out, {
			message: 'the bridge logged the account out'
		})
		.toEqual([ACCOUNT]);
	await expect(page.getByTestId('manage-no-link')).toBeVisible();

	// --- Not connected -----------------------------------------------------
	await page.goto('/networks');
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connected', 'no');
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-linked', 'no');
	await expect(page.getByTestId('state-whatsapp')).toHaveCount(0);
	// And the card offers to connect, not to manage nothing.
	await expect(page.getByTestId('manage-whatsapp')).toHaveCount(0);

	// --- Connect again -----------------------------------------------------
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();
	await bridge.releaseCompletion(ACCOUNT);
	await expect(page.getByTestId('login-complete')).toBeVisible();

	await page.goto('/networks');
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connection', 'connected');
	await expect(page.getByTestId('manage-whatsapp')).toBeVisible();
});

test('a login in flight does not make a connected network look disconnected', async ({
	context,
	page,
	request
}) => {
	// The live incident, reproduced: the bridge holds a connected login *and*
	// a QR scan is running. The old reading answered `awaiting_remote`, which
	// is not `complete`, so the badge disappeared — on a connection that was
	// working perfectly.
	const token = await signIn(context, request, 'the scanning device');
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await bridge.addExistingLogin(ACCOUNT, ACCOUNT_NAME);

	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();

	// The two facts side by side, from the Gateway itself: a login process is
	// mid-scan, and the link is connected. Everything below is the Companion
	// reading the second of them rather than the first.
	const listed = await request.get('/api/bridges', {
		headers: { cookie: `twalk_device=${token}` }
	});
	const row = ((await listed.json()) as { bridges: Record<string, never>[] }).bridges.find(
		(bridge) => bridge['bridge_id'] === WHATSAPP_BRIDGE
	) as unknown as {
		login: { state: string };
		connection: { state: string };
	};
	expect(row.login.state).toBe('awaiting_remote');
	expect(row.connection.state).toBe('connected');

	// The picker, while that scan is still running. Same browser, one tab:
	// the Companion elects a single tab (#67), so a second one would be shown
	// the "open elsewhere" screen instead of the grid.
	await page.goto('/networks');
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connection', 'connected');
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connected', 'yes');
	await expect(page.getByTestId('manage-whatsapp')).toBeVisible();

	// And cancelling that scan does not unlink anything either.
	await clearLogin(request, token, WHATSAPP_BRIDGE);
	await page.reload();
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connection', 'connected');
});

test('a session the network revoked asks to be re-linked, and re-links in place', async ({
	page,
	request
}) => {
	// `BAD_CREDENTIALS` is what a session revoked from the user's own phone
	// reports — never `LOGGED_OUT`, which no mautrix bridge emits. The link
	// still exists, so the card offers *Manage* and not *Connect*: what the
	// user needs is the screen that can re-link it.
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await bridge.addExistingLogin(ACCOUNT, ACCOUNT_NAME);
	await bridge.setLoginState(ACCOUNT, 'BAD_CREDENTIALS');

	await page.goto('/networks');
	const card = page.getByTestId('card-whatsapp');
	await expect(card).toHaveAttribute('data-connection', 'session_expired');
	await expect(card).toHaveAttribute('data-connected', 'no');
	await expect(card).toHaveAttribute('data-linked', 'yes');
	await expect(page.getByTestId('manage-whatsapp')).toBeVisible();

	await page.getByTestId('manage-whatsapp').click();
	await expect(page.getByTestId('manage-state-detail')).toContainText(
		/linked devices|appareils liés/i
	);

	// Re-link is the Gateway's reconnect: the same flow against the login the
	// bridge already holds, never a second one. Getting that wrong is how a
	// network that caps linked devices refuses the repair outright.
	await page.getByTestId('relink').click();
	await expect(page).toHaveURL(new RegExp(`relink=${ACCOUNT}`));
	// The same login screen, so the same ban-risk disclosure: it is dismissed
	// per device and this browser has never seen it.
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();
	await expect
		.poll(async () => (await bridge.stats()).starts.map((start) => start.login_id), {
			message: 'the start carried the existing login id'
		})
		.toEqual([ACCOUNT]);
});
