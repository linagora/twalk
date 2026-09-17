// Screen 3d — an existing Matrix account, against a real Synapse.
//
// The acceptance criterion this file exists for: the rooms are listed **from
// the browser**, and the Gateway learns only the ids the user ticked. So the
// journey is driven through the page — sign in, read the list, tick one of two
// rooms — and then checked on the homeserver itself: the Sensor is invited to
// the ticked room, and is not in the other.

import { expect, test, type APIRequestContext } from '@playwright/test';

import { bridgeStack, NO_STACK, signIn } from './harness';

const stack = bridgeStack();

test.skip(stack === null, NO_STACK);
test.describe.configure({ mode: 'serial' });

const PASSWORD = 'test-only-password-bot_alpha';

/** The owner's Matrix session, from the harness rather than through the page. */
async function ownerToken(request: APIRequestContext): Promise<string> {
	const answer = await request.get('/stub-control/matrix-user?localpart=bot_alpha');
	expect(answer.ok(), await answer.text()).toBeTruthy();
	return ((await answer.json()) as { access_token: string }).access_token;
}

async function createRoom(
	request: APIRequestContext,
	token: string,
	name: string
): Promise<string> {
	const answer = await request.post(`${stack!.synapseUrl}/_matrix/client/v3/createRoom`, {
		headers: { authorization: `Bearer ${token}` },
		data: { name, preset: 'private_chat' }
	});
	expect(answer.ok(), await answer.text()).toBeTruthy();
	return ((await answer.json()) as { room_id: string }).room_id;
}

/** The Sensor's membership in a room, straight from the homeserver. */
async function sensorMembership(
	request: APIRequestContext,
	token: string,
	roomId: string
): Promise<string | null> {
	const answer = await request.get(
		`${stack!.synapseUrl}/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}/state/m.room.member/${encodeURIComponent('@sensor:test.twalk')}`,
		{ headers: { authorization: `Bearer ${token}` } }
	);
	if (!answer.ok()) {
		return null;
	}
	return ((await answer.json()) as { membership?: string }).membership ?? null;
}

test('lists the user’s rooms in the browser, and invites the Sensor into the chosen one', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');
	const token = await ownerToken(request);

	// Two rooms, so "only the ones you ticked" is a claim with something to
	// be false about.
	const stamp = Date.now();
	const chosen = await createRoom(request, token, `Twalk chosen ${stamp}`);
	const untouched = await createRoom(request, token, `Twalk untouched ${stamp}`);

	await page.goto('/networks/matrix');
	await expect(page.getByTestId('screen-matrix')).toHaveAttribute('data-stage', 'signing-in');

	// Sign in against the real homeserver, from the page.
	await page.getByTestId('matrix-homeserver').fill(stack!.synapseUrl);
	await page.getByTestId('matrix-homeserver').blur();
	await expect(page.getByTestId('matrix-username')).toBeVisible();
	await page.getByTestId('matrix-username').fill('bot_alpha');
	await page.getByTestId('matrix-password').fill(PASSWORD);
	await page.getByTestId('matrix-signin').click();

	// The list came from the homeserver, in this browser.
	const list = page.getByTestId('matrix-rooms');
	await expect(list).toBeVisible();
	await expect(list).toContainText(`Twalk chosen ${stamp}`);
	await expect(list).toContainText(`Twalk untouched ${stamp}`);

	// Nothing has been sent to the Gateway yet — the screen says as much, and
	// says it before the user ticks anything.
	await expect(page.getByTestId('screen-matrix')).toContainText(
		/only learns the ones you tick|ne connaîtra que ceux que vous cochez/
	);

	await page.getByTestId(`room-${chosen}`).check();
	await page.getByTestId('invite-sensor').click();

	const invited = page.getByTestId('matrix-invited');
	await expect(invited).toBeVisible();
	await expect(page.getByTestId(`outcome-${chosen}`)).toHaveAttribute('data-status', 'invited');
	await expect(invited).toContainText('@sensor:test.twalk');

	// The homeserver's own answer, which is the one that counts.
	await expect.poll(async () => sensorMembership(request, token, chosen)).toBe('invite');
	expect(await sensorMembership(request, token, untouched)).toBeNull();

	// And the untouched room was never named to the Gateway: it is not among
	// the outcomes it reported.
	await expect(page.getByTestId(`outcome-${untouched}`)).toHaveCount(0);
});

test('asking again about a room the Sensor is already in is not an error', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');
	const token = await ownerToken(request);
	const stamp = Date.now();
	const room = await createRoom(request, token, `Twalk twice ${stamp}`);

	for (const pass of [1, 2]) {
		await page.goto('/networks/matrix');
		await page.getByTestId('matrix-homeserver').fill(stack!.synapseUrl);
		await page.getByTestId('matrix-homeserver').blur();
		await page.getByTestId('matrix-username').fill('bot_alpha');
		await page.getByTestId('matrix-password').fill(PASSWORD);
		await page.getByTestId('matrix-signin').click();
		await expect(page.getByTestId('matrix-rooms')).toBeVisible();
		await page.getByTestId(`room-${room}`).check();
		await page.getByTestId('invite-sensor').click();
		await expect(page.getByTestId(`outcome-${room}`)).toHaveAttribute(
			'data-status',
			pass === 1 ? 'invited' : 'already_present'
		);
	}
});

test('a refused sign-in says which of the homeserver’s refusals it was', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');
	await page.goto('/networks/matrix');
	await page.getByTestId('matrix-homeserver').fill(stack!.synapseUrl);
	await page.getByTestId('matrix-homeserver').blur();
	await page.getByTestId('matrix-username').fill('bot_alpha');
	await page.getByTestId('matrix-password').fill('not-the-password');
	await page.getByTestId('matrix-signin').click();

	const problem = page.getByTestId('matrix-problem');
	await expect(problem).toBeVisible();
	await expect(problem).toContainText(/credentials|identifiants/);
	// The password field is cleared, and nothing was written anywhere.
	await expect(page.getByTestId('matrix-password')).toHaveValue('');
	const stored = await page.evaluate(() => JSON.stringify({ ...localStorage }));
	expect(stored).not.toContain('not-the-password');
});
