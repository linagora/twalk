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

test('the search filters the list, and the bulk control touches only what it shows', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');
	const token = await ownerToken(request);

	// Three rooms, two of which a search can tell apart from the third. On a
	// real work account this list is a hundred rooms of colleagues' private
	// conversations, which is why the bulk control is scoped (#137, #122).
	const stamp = Date.now();
	const alpha = await createRoom(request, token, `Twalk alpha ${stamp}`);
	const beta = await createRoom(request, token, `Twalk beta ${stamp}`);
	const gamma = await createRoom(request, token, `Other gamma ${stamp}`);

	await page.goto('/networks/matrix');
	await page.getByTestId('matrix-homeserver').fill(stack!.synapseUrl);
	await page.getByTestId('matrix-homeserver').blur();
	await page.getByTestId('matrix-username').fill('bot_alpha');
	await page.getByTestId('matrix-password').fill(PASSWORD);
	await page.getByTestId('matrix-signin').click();

	const list = page.getByTestId('matrix-rooms');
	await expect(list).toBeVisible();
	await expect(page.getByTestId(`room-${gamma}`)).toBeVisible();

	// The search narrows the list to what was asked for.
	await page.getByTestId('matrix-room-search').fill(`Twalk alpha ${stamp}`);
	await expect(page.getByTestId(`room-${alpha}`)).toBeVisible();
	await expect(page.getByTestId(`room-${beta}`)).toHaveCount(0);
	await expect(page.getByTestId(`room-${gamma}`)).toHaveCount(0);

	// The bulk control names what it will do, and does only that. This is the
	// assertion that matters: a room the filter is hiding must not be chosen
	// by a click the user could not see the consequence of.
	const bulk = page.getByTestId('matrix-rooms-toggle-shown');
	await expect(bulk).toContainText(/1/);
	await bulk.click();
	await expect(page.getByTestId(`room-${alpha}`)).toBeChecked();

	await page.getByTestId('matrix-room-search').fill('');
	await expect(page.getByTestId(`room-${beta}`)).not.toBeChecked();
	await expect(page.getByTestId(`room-${gamma}`)).not.toBeChecked();
	await expect(page.getByTestId(`room-${alpha}`)).toBeChecked();

	// And it undoes what it did, over the same scope.
	await page.getByTestId('matrix-room-search').fill(`Twalk alpha ${stamp}`);
	await expect(bulk).toContainText(/1/);
	await bulk.click();
	await expect(page.getByTestId(`room-${alpha}`)).not.toBeChecked();
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

test('an SSO-only homeserver gets its provider’s own button and no password form', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');

	// The owner's own deployment answers exactly this: three flows, no
	// password. Only the flow listing is stubbed — the rest of the screen is
	// the real one, and a homeserver with an identity provider is not
	// something the test stack can be made into.
	await page.route(`${stack!.synapseUrl}/_matrix/client/v3/login`, async (route) => {
		if (route.request().method() !== 'GET') {
			await route.continue();
			return;
		}
		await route.fulfill({
			json: {
				flows: [
					{
						type: 'm.login.sso',
						identity_providers: [{ id: 'oidc-twake', name: 'Connect with Twake' }]
					},
					{ type: 'm.login.token' },
					{ type: 'm.login.application_service' }
				]
			}
		});
	});

	await page.goto('/networks/matrix');
	await page.getByTestId('matrix-homeserver').fill(stack!.synapseUrl);
	await page.getByTestId('matrix-homeserver').blur();

	// The provider's own name, and its id in the redirect so the homeserver's
	// chooser page is skipped.
	const button = page.getByTestId('matrix-sso-oidc-twake');
	await expect(button).toBeVisible();
	await expect(button).toHaveText(/Connect with Twake/);

	// No password form, no generic SSO button, and nothing that reads as
	// broken for their absence.
	await expect(page.getByTestId('matrix-username')).toHaveCount(0);
	await expect(page.getByTestId('matrix-password')).toHaveCount(0);
	await expect(page.getByTestId('matrix-sso')).toHaveCount(0);
	await expect(page.getByTestId('matrix-no-flow')).toHaveCount(0);
	await expect(page.getByTestId('matrix-problem')).toHaveCount(0);
});

test('an SSO round trip keeps its homeserver, and refuses the token anywhere else', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');

	// The journey this pins was found live against a corporate homeserver: the
	// redirect comes back as a full page load, component state is gone, and
	// the screen used to rebuild the homeserver from what onboarding had
	// remembered — the *deployment's* server — and present a token issued by
	// one homeserver to a different one (#125).
	//
	// Arriving with a token and nothing remembered is the same situation with
	// the fallback removed: there is no issuer to exchange it against.
	await page.goto('/networks/matrix?loginToken=not-a-real-token');

	// It says so, and it says nothing was sent anywhere — because nothing was.
	await expect(page.getByTestId('matrix-problem')).toBeVisible();
	await expect(page.getByTestId('matrix-problem')).toContainText(/no longer knows which homeserver/i);

	// And the credential is out of the address bar whatever happened next.
	// It used to be stripped only on the path that went on to use it, so a
	// round trip that could not complete left it there.
	expect(page.url()).not.toContain('loginToken');

	// No request carried the token to any homeserver.
	const carried: string[] = [];
	page.on('request', (r) => {
		if (r.url().includes('/login') || (r.postData() ?? '').includes('not-a-real-token')) {
			carried.push(`${r.method()} ${r.url()}`);
		}
	});
	await page.waitForTimeout(500);
	expect(carried, 'the token was presented to no one').toEqual([]);
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

test('a refused invitation is readable from where the button was pressed', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');

	// A hundred rooms, which is what a work account looks like and what this
	// screen was unusable with (#139). The list is stubbed at the homeserver's
	// own `/sync` rather than created for real: what is under test is where the
	// refusal renders, and a hundred `createRoom` calls would prove nothing
	// about that while taking a minute.
	// Matched by path rather than by a glob: the request carries a JSON filter
	// in its query string, and a pattern that has to survive that is a pattern
	// that silently stops matching.
	await page.route(
		(url) => url.pathname === '/_matrix/client/v3/sync',
		async (route) => {
			const join: Record<string, unknown> = {};
			for (let index = 0; index < 100; index += 1) {
				const number = String(index).padStart(3, '0');
				join[`!room${number}:test.twalk`] = {
					state: {
						events: [{ type: 'm.room.name', content: { name: `Room ${number}` } }]
					},
					timeline: { events: [] },
					summary: {}
				};
			}
			await route.fulfill({ json: { rooms: { join } } });
		}
	);

	// And a refusal to answer the click with. The Gateway's own 401 is what
	// the owner met; stubbing it here keeps the assertion about the screen.
	await page.route('**/api/bootstrap/rooms', async (route) => {
		await route.fulfill({ status: 401, json: { error: 'matrix_token_rejected' } });
	});

	await page.goto('/networks/matrix');
	await page.getByTestId('matrix-homeserver').fill(stack!.synapseUrl);
	await page.getByTestId('matrix-homeserver').blur();
	await page.getByTestId('matrix-username').fill('bot_alpha');
	await page.getByTestId('matrix-password').fill(PASSWORD);
	await page.getByTestId('matrix-signin').click();

	await expect(page.getByTestId('matrix-rooms')).toBeVisible();
	await expect(page.getByTestId('matrix-rooms-count')).toContainText('100');

	// Act at the bottom, as the owner did: tick a room and press the button,
	// which Playwright scrolls to exactly as a finger would.
	await page.getByTestId('room-!room000:test.twalk').check();
	const button = page.getByTestId('invite-sensor');
	await button.click();

	// The answer is where the action was. `toBeInViewport` is the assertion
	// that could have caught this: the old message was visible to a selector
	// and three thousand pixels above the button to a person.
	const answer = page.getByTestId('matrix-invite-problem');
	await expect(answer).toBeVisible();
	await expect(answer).toBeInViewport();
	await expect(button).toBeInViewport();
	await expect(answer).toHaveAttribute('role', 'alert');
	await expect(answer).toContainText(/homeserver|serveur/i);

	// And it did not also render at the top, where nobody was looking.
	await expect(page.getByTestId('matrix-problem')).toHaveCount(0);
});

test('the homeserver field starts empty, whatever onboarding resolved', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');

	// A browser that has walked the bootstrap journey: it knows the Twalk
	// domain and the homeserver onboarding resolved. Both are the deployment's
	// own server, which is the one account this screen is not for — and both
	// are what used to arrive in the field (#124).
	await page.addInitScript(() => {
		window.localStorage.setItem('twalk:domain', 'twalk.localhost:8009');
		window.localStorage.setItem('twalk:homeserver', 'http://twalk.localhost:8009');
	});

	await page.goto('/networks/matrix');
	const field = page.getByTestId('matrix-homeserver');
	await expect(field).toBeVisible();
	await expect(field).toHaveValue('');

	// Nothing arrives that could only have come from the deployment, and no
	// login form is offered for a homeserver nobody named.
	await expect(page.getByTestId('screen-matrix')).not.toContainText('twalk.localhost');
	await expect(page.getByTestId('matrix-username')).toHaveCount(0);
	await expect(page.getByTestId('matrix-password')).toHaveCount(0);
	await expect(page.getByTestId('matrix-sso')).toHaveCount(0);

	// The shape of what to type is on the screen rather than in the field.
	await expect(field).toHaveAttribute('placeholder', /\./);
});

test('a server name is enough: the field follows .well-known delegation', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');

	// `@mmaudet:linagora.com` is what a person can recite; `linagora.com`
	// delegates to `matrix.linagora.com`, and the field used to fail on the
	// first while working on the second (#124). The delegation is stubbed —
	// the homeserver behind it is the real Synapse, and everything after this
	// one document is the real journey.
	await page.route('https://delegated.test/.well-known/matrix/client', async (route) => {
		await route.fulfill({
			status: 200,
			headers: { 'access-control-allow-origin': '*', 'content-type': 'application/json' },
			body: JSON.stringify({ 'm.homeserver': { base_url: stack!.synapseUrl } })
		});
	});

	await page.goto('/networks/matrix');
	await page.getByTestId('matrix-homeserver').fill('delegated.test');
	await page.getByTestId('matrix-homeserver-continue').click();

	// It says where the delegation led, because the credentials are about to
	// go somewhere the user did not type.
	const resolved = page.getByTestId('matrix-resolved');
	await expect(resolved).toBeVisible();
	await expect(resolved).toHaveAttribute('data-homeserver', stack!.synapseUrl);

	// And the flows are that server's: this is the real Synapse answering.
	await page.getByTestId('matrix-username').fill('bot_alpha');
	await page.getByTestId('matrix-password').fill(PASSWORD);
	await page.getByTestId('matrix-signin').click();
	await expect(page.getByTestId('screen-matrix')).toHaveAttribute('data-stage', 'rooms');
});

test('a homeserver that answers nothing is named as that, before any credential', async ({
	context,
	page,
	request
}) => {
	await signIn(context, request, 'the Matrix device');
	await page.goto('/networks/matrix');
	await page.getByTestId('matrix-homeserver').fill('nothing-here.invalid');
	await page.getByTestId('matrix-homeserver-continue').click();

	const problem = page.getByTestId('matrix-problem');
	await expect(problem).toBeVisible({ timeout: 20_000 });
	await expect(problem).toContainText(/nothing-here.invalid/);
	// No form for a server that is not there.
	await expect(page.getByTestId('matrix-username')).toHaveCount(0);
});
