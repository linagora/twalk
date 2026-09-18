// What screens 4 and 5 need on top of the network journeys' own harness: a
// connected network to activate a persona on, and a second signed-in device to
// revoke.

import { expect, type APIRequestContext, type Page } from '@playwright/test';

import { StubBridge, WHATSAPP_BRIDGE } from '../networks/harness';

export { bridgeStack, clearLogin, NO_STACK, signIn, StubBridge, WHATSAPP_BRIDGE } from '../networks/harness';

/**
 * Connects WhatsApp the way a user does — screen 3a, disclosure and all — and
 * leaves the deployment with exactly one network a persona can be activated
 * on.
 *
 * Through the browser rather than through the API, because the state screen 4
 * reads is the state screen 3a leaves behind, and a test that wrote it
 * directly would prove the two agree only in its own imagination.
 */
export async function connectWhatsApp(page: Page, request: APIRequestContext): Promise<void> {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();
	await bridge.releaseCompletion(CONNECTED_LOGIN);
	await expect(page.getByTestId('login-complete')).toBeVisible();
}

/** The login the bridge is left holding once that journey has run. */
export const CONNECTED_LOGIN = '33612345678';

/** One more signed-in device, and the ids a test needs to talk about it. */
export interface ExtraDevice {
	id: string;
	name: string;
	token: string;
}

/**
 * Signs a second device in without touching the browser's own session.
 *
 * The device token comes back to the test rather than as a cookie, because the
 * Gateway sets it `HttpOnly` — and here that is exactly what the test wants: a
 * token it can present by hand, before and after the revocation, to see the
 * session refused.
 */
export async function signInExtraDevice(
	request: APIRequestContext,
	name: string
): Promise<ExtraDevice> {
	const answer = await request.post(`/stub-control/sign-in?device=${encodeURIComponent(name)}`);
	expect(answer.ok(), await answer.text()).toBeTruthy();
	const body = (await answer.json()) as {
		device_token: string;
		session: { device: { id: string; name: string } };
	};
	return { id: body.session.device.id, name: body.session.device.name, token: body.device_token };
}

/** `GET /api/session` as that device, which is the whole question of a revocation. */
export async function sessionStatus(
	request: APIRequestContext,
	token: string
): Promise<number> {
	const answer = await request.get('/api/session', {
		headers: { cookie: `twalk_device=${token}` }
	});
	return answer.status();
}

/** The recorded consent state, read as the owner. */
export async function consentState(
	request: APIRequestContext,
	token: string
): Promise<
	{
		subject: { type: string; id: string };
		network: string;
		state: string;
	}[]
> {
	const answer = await request.get('/api/consent/state', {
		headers: { cookie: `twalk_device=${token}` }
	});
	expect(answer.ok(), await answer.text()).toBeTruthy();
	return ((await answer.json()) as { entries: never[] }).entries;
}

/**
 * How many contacts the Gateway says are waiting for a decision.
 *
 * Read for the number alone. The same answer carries the contacts themselves,
 * and a spec that pulled them out to assert on would be building the document
 * screen 5 exists not to render.
 */
export async function pendingTotal(
	request: APIRequestContext,
	token: string
): Promise<number> {
	const answer = await request.get('/api/contacts/pending', {
		headers: { cookie: `twalk_device=${token}` }
	});
	expect(answer.ok(), await answer.text()).toBeTruthy();
	return ((await answer.json()) as { total: number }).total;
}

/**
 * Loses the bridge's session, the way one is really lost: the bridge reports
 * `BAD_CREDENTIALS` for the login it holds — the wireframe's amber state.
 *
 * That, and not a failed login attempt, is what an expired session is. A
 * session revoked from the user's own phone reports `BAD_CREDENTIALS`; no
 * mautrix bridge emits `LOGGED_OUT` at all (#56's mapping table). This helper
 * used to start a login and have it refused with a `410`, which made the
 * dashboard's amber banner a statement about a *login process* rather than
 * about the link — the confusion #108 is about.
 */
export async function expireWhatsAppSession(
	request: APIRequestContext,
	token: string
): Promise<void> {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	const cookie = `twalk_device=${token}`;
	await bridge.setLoginState(CONNECTED_LOGIN, 'BAD_CREDENTIALS');

	await expect
		.poll(async () => {
			const answer = await request.get('/api/bridges', { headers: { cookie } });
			const body = (await answer.json()) as {
				bridges: { network: string; connection: { state: string | null } }[];
			};
			return body.bridges.find((entry) => entry.network === 'whatsapp')?.connection.state ?? 'none';
		})
		.toBe('session_expired');
}
