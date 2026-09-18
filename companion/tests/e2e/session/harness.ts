// What the session journeys of ticket #111 need: a browser holding a real
// pair of session cookies, and a way to kill them at the Gateway.
//
// The origin is the session stack's (`tests/real-stack.mjs`,
// `startSessionStack`), whose device token lives five seconds. Everything else
// about it is a deployment: a real Gateway, a real Synapse, real cookies with
// the flags the Gateway sets, and a stub only where a bridge would need a
// human with a phone.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, type APIRequestContext, type BrowserContext, type Page } from '@playwright/test';

export const WHATSAPP_BRIDGE = 'mautrix-whatsapp';

export interface SessionStack {
	owner: string;
	ownerId: string;
	serverName: string;
	synapseUrl: string;
	domain: string;
	gatewayOrigin: string;
	stubOrigin: string;
	/** What `GATEWAY_DEVICE_TOKEN_TTL` was set to for this origin. */
	deviceTokenTtlSeconds: number;
}

const STACK_FILE = join(
	dirname(fileURLToPath(import.meta.url)),
	'..',
	'..',
	'.session-stack.json'
);

/** What this run's session Gateway is, or `null` when there is none. */
export function sessionStack(): SessionStack | null {
	if (process.env.TWALK_TEST_REAL_STACK !== '1') {
		return null;
	}
	try {
		return JSON.parse(readFileSync(STACK_FILE, 'utf8')) as SessionStack;
	} catch {
		return null;
	}
}

export const NO_STACK =
	'needs a real Gateway and a real Synapse: run `npm run test:e2e:stack` (Docker and cargo required)';

export interface SignedInDevice {
	deviceId: string;
	deviceToken: string;
	refreshToken: string;
}

/**
 * Signs a device in and hands back both of its tokens.
 *
 * Both, because #111 is about the pair: a browser given only `twalk_device`
 * can never refresh, so a test that set only that one would be asserting a
 * session this product does not issue. The tokens come back to the test rather
 * than as cookies the browser already holds because the Gateway sets them
 * `HttpOnly` and a spec cannot read them out of a page; what reaches the
 * Gateway is identical either way.
 */
export async function signInDevice(
	request: APIRequestContext,
	deviceName: string
): Promise<SignedInDevice> {
	const answer = await request.post(`/stub-control/sign-in?device=${encodeURIComponent(deviceName)}`);
	expect(answer.ok(), await answer.text()).toBeTruthy();
	const body = (await answer.json()) as {
		device_token: string;
		refresh_token: string | null;
		session: { device: { id: string } };
	};
	expect(body.refresh_token, 'the sign-in set a refresh cookie').not.toBeNull();
	return {
		deviceId: body.session.device.id,
		deviceToken: body.device_token,
		refreshToken: body.refresh_token as string
	};
}

/**
 * Puts a signed-in device's cookies on the browser, exactly as the Gateway
 * scopes them: the device token on the whole origin, the refresh token on
 * `/api/session` alone (`companion-gateway/openapi.yaml`).
 *
 * The path matters and is not cosmetic. A refresh cookie set on `/` would ride
 * on every request and every static file, and a test that set it that way
 * would be proving a client the Gateway does not serve.
 */
export async function useDevice(context: BrowserContext, device: SignedInDevice): Promise<void> {
	await context.addCookies([
		{
			name: 'twalk_device',
			value: device.deviceToken,
			domain: '127.0.0.1',
			path: '/',
			httpOnly: true,
			sameSite: 'Lax'
		},
		{
			name: 'twalk_refresh',
			value: device.refreshToken,
			domain: '127.0.0.1',
			path: '/api/session',
			httpOnly: true,
			sameSite: 'Lax'
		}
	]);
}

/**
 * What a browser that has onboarded here remembers: the deployment it belongs
 * to. Written before the page loads, because these journeys arrive on a screen
 * directly rather than walking screen 1 first.
 */
export async function rememberDeployment(context: BrowserContext, stack: SessionStack) {
	await context.addInitScript(
		([domain, homeserver]) => {
			try {
				window.localStorage.setItem('twalk:domain', domain);
				window.localStorage.setItem('twalk:homeserver', homeserver);
			} catch {
				// A browser with storage off still works; the dialog asks.
			}
		},
		[stack.domain, stack.synapseUrl]
	);
}

/**
 * Revokes one device at the Gateway, which drops **both** of its token digests
 * — the device token and the refresh token together.
 *
 * That is how "both credentials are dead" is arranged without waiting for the
 * refresh token's thirty days: the device token is already expired by then on
 * this origin, and this takes the other half. It is also a real thing that
 * happens to a user — the dashboard's device list revokes, and so does signing
 * out elsewhere — so the state under test is one the product produces.
 */
export async function revokeDevice(
	request: APIRequestContext,
	device: SignedInDevice,
	using: SignedInDevice
) {
	const answer = await request.delete(`/api/devices/${device.deviceId}`, {
		headers: { cookie: `twalk_device=${using.deviceToken}` }
	});
	expect(answer.status(), await answer.text()).toBe(204);
}

/** Long enough for this origin's device token to have died at the Gateway. */
export async function waitForTokenToExpire(page: Page, stack: SessionStack) {
	// A second of slack over the configured lifetime: the Gateway compares
	// against its own clock, and a test that raced it would flake.
	await page.waitForTimeout((stack.deviceTokenTtlSeconds + 1) * 1000);
}

/** Every response this page received, so a journey can assert what happened. */
export class Traffic {
	readonly seen: { url: string; status: number; method: string }[] = [];

	constructor(page: Page) {
		page.on('response', (response) => {
			this.seen.push({
				url: new URL(response.url()).pathname,
				status: response.status(),
				method: response.request().method()
			});
		});
	}

	get refusals() {
		return this.seen.filter((entry) => entry.status === 401 && entry.url.startsWith('/api/'));
	}

	get refreshes() {
		return this.seen.filter(
			(entry) => entry.url === '/api/session/refresh' && entry.method === 'POST'
		);
	}

	get successfulRefreshes() {
		return this.refreshes.filter((entry) => entry.status === 200);
	}

	clear() {
		this.seen.length = 0;
	}
}

/**
 * Connects WhatsApp on this deployment, through the Gateway and the stub
 * bridge, so that a later screen has a connected network to still be connected.
 */
export async function connectWhatsApp(page: Page, request: APIRequestContext) {
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();
	const released = await request.post(`/stub-control/${WHATSAPP_BRIDGE}/release-complete`, {
		data: { login_id: '33600000111' }
	});
	expect(released.ok(), await released.text()).toBeTruthy();
	await expect(page.getByTestId('login-complete')).toBeVisible();
}
