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
	await bridge.releaseCompletion('33612345678');
	await expect(page.getByTestId('login-complete')).toBeVisible();
}

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
 * Loses the bridge's session: a login started on the bridge and refused with
 * the `410` mautrix answers when it has ended one, which the Gateway records
 * as `login_expired` — the wireframe's amber state.
 *
 * Driven through the Gateway's API rather than through screen 3a, because the
 * screen under test here is the dashboard. What it has to show is a bridge
 * state the Gateway holds; how that state came to be is screen 3a's own
 * journey, and `tests/e2e/networks/whatsapp.spec.ts` walks it.
 */
export async function expireWhatsAppSession(
	request: APIRequestContext,
	token: string
): Promise<void> {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	const cookie = `twalk_device=${token}`;
	const started = request.post(`/api/bridges/${WHATSAPP_BRIDGE}/login`, {
		headers: { cookie },
		data: { flow_id: 'qr' }
	});
	// The Gateway holds the bridge's blocking step itself, so the refusal is
	// queued while that request is in flight rather than before it.
	await expect
		.poll(async () => (await bridge.stats()).blocking_arrivals)
		.toBeGreaterThanOrEqual(1);
	await bridge.releaseRefusal(410, 'LOGIN_TIMED_OUT');
	await started;

	await expect
		.poll(async () => {
			const answer = await request.get('/api/bridges', { headers: { cookie } });
			const body = (await answer.json()) as {
				bridges: {
					network: string;
					login: { state: string; error: { code: string } | null } | null;
				}[];
			};
			const row = body.bridges.find((entry) => entry.network === 'whatsapp');
			return row?.login?.error?.code ?? row?.login?.state ?? 'none';
		})
		.toBe('login_expired');
}
