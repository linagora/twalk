// What every networks journey needs: a signed-in device, and a handle on the
// stub bridge's side of the login.
//
// Both reach the same origin the Companion is served from —
// `tests/serve-like-gateway.mjs`, in its bridge-stack mode, proxies `/api/*` to
// the real Gateway and `/stub-control/*` to the stub bridge — so a test never
// has to know which port anything ended up on.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, type APIRequestContext, type BrowserContext, type Page } from '@playwright/test';

export const WHATSAPP_BRIDGE = 'mautrix-whatsapp';
export const SIGNAL_BRIDGE = 'mautrix-signal';
export const SMS_BRIDGE = 'mautrix-gmessages';

/** The bridge stack this run brought up, or `null` when there is none. */
export interface BridgeStack {
	owner: string;
	ownerId: string;
	serverName: string;
	synapseUrl: string;
	gatewayOrigin: string;
	stubOrigin: string;
}

const STACK_FILE = join(
	dirname(fileURLToPath(import.meta.url)),
	'..',
	'..',
	'.bridge-stack.json'
);

/**
 * What `tests/real-stack.mjs` wrote about this run's bridge Gateway, or `null`.
 *
 * The networks specs skip themselves without it, exactly as the bootstrap ones
 * do (`tests/e2e/stack.ts`): `npm test` stays a Node-only suite that needs
 * neither Docker nor a Rust toolchain, and `npm run test:e2e:stack` is the one
 * that proves the journeys. A spec that silently passed without the stack would
 * be worse than one that says it did not run.
 */
export function bridgeStack(): BridgeStack | null {
	if (process.env.TWALK_TEST_REAL_STACK !== '1') {
		return null;
	}
	try {
		return JSON.parse(readFileSync(STACK_FILE, 'utf8')) as BridgeStack;
	} catch {
		return null;
	}
}

export const NO_STACK =
	'needs a real Gateway, a real Synapse and the stub bridge: run `npm run test:e2e:stack` (Docker and cargo required)';

/**
 * Signs a device in and puts its token on the browser context.
 *
 * The Gateway issues the token as an `HttpOnly` cookie, so a test cannot read
 * it back out of a page. It asks the harness for one instead — the harness is
 * what holds the Matrix password the OpenID token is minted from — and sets the
 * cookie itself. What reaches the Gateway is identical either way.
 */
export async function signIn(
	context: BrowserContext,
	request: APIRequestContext,
	deviceName: string
): Promise<string> {
	const answer = await request.post(`/stub-control/sign-in?device=${encodeURIComponent(deviceName)}`);
	expect(answer.ok(), await answer.text()).toBeTruthy();
	const { device_token } = (await answer.json()) as { device_token: string };
	await context.addCookies([
		{
			name: 'twalk_device',
			value: device_token,
			domain: '127.0.0.1',
			path: '/',
			httpOnly: true,
			sameSite: 'Lax'
		}
	]);
	return device_token;
}

/**
 * Cancels whatever the previous test — or a hand-driven probe — left in flight.
 *
 * The Gateway is one process for the whole suite and a login lives in its
 * memory, so "no login in flight" is a precondition a test has to establish,
 * not one it inherits. A `404` here means there was nothing to cancel, which is
 * the state we wanted.
 */
export async function clearLogin(request: APIRequestContext, token: string, bridgeId: string) {
	await request.delete(`/api/bridges/${bridgeId}/login`, {
		headers: { cookie: `twalk_device=${token}` }
	});
}

/** The stub bridge's control surface, for one bridge instance. */
export class StubBridge {
	constructor(
		private readonly request: APIRequestContext,
		private readonly bridgeId: string
	) {}

	/** Answers the held blocking step with a refreshed code — a bumped generation. */
	async releaseRefreshedQr(data?: string) {
		await this.post('release-qr', data === undefined ? {} : { data });
	}

	/** Answers the held blocking step with the completion the phone's scan causes. */
	async releaseCompletion(loginId = 'stub-login-complete') {
		await this.post('release-complete', { login_id: loginId });
	}

	/** Answers it with one of mautrix's own refusals. */
	async releaseRefusal(status: number, errcode: string) {
		await this.post('release-refusal', { status, errcode });
	}

	/** A bridge restart: every process forgotten, every held request 404ed. */
	async restart() {
		await this.post('restart', {});
	}

	async addExistingLogin(loginId: string, name: string) {
		await this.post('add-login', { login_id: loginId, name });
	}

	async reset() {
		await this.post('reset', {});
	}

	async stats(): Promise<{
		blocking_arrivals: number;
		held: number;
		starts: { flow_id: string; user_id: string | null; login_id: string | null }[];
		/** Every step body the Gateway relayed: how a credential is proved to have passed *through*. */
		submits: {
			step_id: string;
			step_type: string;
			body: { cookies?: Record<string, string> } | null;
		}[];
		cancelled: string[];
		logins: { id: string }[];
	}> {
		const answer = await this.request.get(`/stub-control/${this.bridgeId}/stats`);
		expect(answer.ok(), await answer.text()).toBeTruthy();
		return answer.json();
	}

	private async post(action: string, body: Record<string, unknown>) {
		const answer = await this.request.post(`/stub-control/${this.bridgeId}/${action}`, {
			data: body
		});
		expect(answer.ok(), await answer.text()).toBeTruthy();
	}
}

/** The payload currently drawn, identified by the SVG's module count and path. */
export async function drawnCode(page: Page): Promise<string> {
	const code = page.getByTestId('qr-code');
	await expect(code).toBeVisible();
	return (await code.locator('path').getAttribute('d')) ?? '';
}
