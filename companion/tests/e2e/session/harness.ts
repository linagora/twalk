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

// # This suite's tolerance, and why it has one (#186)
//
// These journeys are the only ones in the Companion's suite whose subject is
// *when* something happened, so they are the only ones that can lose to a slow
// machine. Naming the tolerance is half of not being beaten by it.
//
// The client renews a token with a fifth of its lifetime still in hand
// (`refreshAfterSeconds`, `src/lib/session/refresh.ts`, unit-tested in
// `src/lib/session/session.test.ts`). So on this origin:
//
//   lifetime   15 s   `GATEWAY_DEVICE_TOKEN_TTL` (`tests/real-stack.mjs`)
//   renewed at 12 s   lifetime − margin
//   headroom    3 s   the margin, and the tolerance of every wait below
//
// **Three seconds** is what a browser timer, a refresh round trip to a real
// Gateway and this process observing the answer have between them before the
// credential is actually dead. That is the number to change if these specs ever
// go red for timing again — and the number *not* to change is the fraction, which
// is the product's and is right (a deployment's fifteen-minute token gets three
// minutes). It was five seconds' lifetime and therefore one second's headroom,
// which is #186.
//
// The other half of not being beaten by it is not creating the load: this project
// runs last in `playwright.config.ts`, after `portals`, so nothing else of this
// suite is compiling or syncing while these four journeys are counting seconds.

/** The client's own headroom on this origin, in milliseconds. See above. */
export function headroomMs(stack: SessionStack): number {
	// `refreshAfterSeconds`'s margin, restated: a fifth of the lifetime, at least
	// a second, at most a minute. Restated rather than imported because that
	// module reaches the generated client through SvelteKit's `$lib` alias, which
	// Playwright does not resolve — so if the two ever disagree, the first spec
	// below is what says so, by finding fewer renewals than lifetimes.
	const margin = Math.max(1, Math.min(60, stack.deviceTokenTtlSeconds / 5));
	return margin * 1000;
}

/** When the client is due to renew a freshly issued token, in milliseconds. */
export function renewedAfterMs(stack: SessionStack): number {
	return Math.max(1000, stack.deviceTokenTtlSeconds * 1000 - headroomMs(stack));
}

/**
 * How long one of these journeys may take, stated rather than inherited.
 *
 * This project had no budget of its own, so the default thirty seconds was
 * carrying the longest journey here — which sits and waits out three whole token
 * lifetimes on purpose. At a five-second lifetime that sleep alone was eighteen
 * seconds, leaving about ten for a real-stack sign-in, an app load, two screens
 * and the assertions: a margin nobody had chosen, in the one project whose specs
 * are allowed to be slow. When it ran out, the failure was a timeout on a spec
 * about refreshing sessions, which is #186's whole shape — an unnamed number
 * deciding whether the suite is believed.
 *
 * So the budget is the longest deliberate wait plus a minute for the browser work
 * and the round trips to a real Gateway and a real Synapse. It is a *ceiling*, not
 * a delay: every journey below finishes well inside it.
 */
export function journeyBudgetMs(stack: SessionStack): number {
	return 3 * stack.deviceTokenTtlSeconds * 1000 + headroomMs(stack) + 60_000;
}

/**
 * Waits until the Gateway actually refuses this browser's credential.
 *
 * Measured rather than timed, which is the point. The old version slept for the
 * configured lifetime plus a second and *assumed* the token was dead — but the
 * page rotates its token whenever it likes, so "the lifetime, from now" was never
 * a bound on when the one it currently holds expires. When the assumption was
 * wrong the next click simply succeeded, and the spec failed for having found no
 * refusal to repair, which reads like the repair being broken.
 *
 * The probe is a bare `fetch` from inside the page, so it carries the browser's
 * own cookies and goes nowhere near the client wrapper that would repair a `401`.
 * Its own refusals land in [`Traffic`] like any other, so a caller that asserts on
 * refusal counts clears them afterwards — which is what the journeys that use this
 * do.
 */
export async function waitUntilTheGatewayRefusesThisBrowser(page: Page, stack: SessionStack) {
	// The bound: a token rotated the instant before refreshes stopped still has a
	// whole lifetime to run, so the worst case is a renewal interval plus a
	// lifetime, plus this suite's headroom.
	const deadline = renewedAfterMs(stack) + stack.deviceTokenTtlSeconds * 1000 + headroomMs(stack);
	const ask = () =>
		page.evaluate(() => fetch('/api/session', { cache: 'no-store' }).then((answer) => answer.status));
	await expect
		.poll(ask, {
			timeout: deadline,
			intervals: [250],
			message:
				`this origin's device token lives ${stack.deviceTokenTtlSeconds}s and the browser's ` +
				'was expected to be dead by now: either the page is still renewing it, or the ' +
				'Gateway is not applying GATEWAY_DEVICE_TOKEN_TTL (tests/real-stack.mjs)'
		})
		.toBe(401);
}

/**
 * Waits until the moment the page had scheduled its renewal for has gone by in
 * real time.
 *
 * For the sleeping tab: its clock is frozen, so nothing it scheduled will fire
 * however long this waits — and what the spec then asserts is exactly that. The
 * wait is the renewal interval and this suite's headroom, so "it did not refresh"
 * is a statement about a moment that has passed rather than about one that has not
 * arrived yet.
 */
export async function waitPastTheScheduledRenewal(page: Page, stack: SessionStack) {
	await page.waitForTimeout(renewedAfterMs(stack) + headroomMs(stack));
}

/**
 * Every request this page issued and every response it received, so a journey can
 * assert what happened.
 *
 * **Requests and responses, not responses alone**, and the difference is a flake
 * (#186). "A sleeping tab refreshes nothing" is a statement about the tab
 * *issuing* a refresh — a timer firing — and the old version could only count
 * answers. A refresh already in flight when the clock was frozen had its answer
 * arrive during the sleep and was counted as a refresh the sleeping tab had made,
 * so the assertion failed on the one case it was written to allow.
 */
export class Traffic {
	readonly seen: { url: string; status: number; method: string }[] = [];
	/** What the page asked for, in order, whatever came back. */
	readonly sent: { url: string; method: string }[] = [];

	constructor(page: Page) {
		page.on('request', (request) => {
			this.sent.push({
				url: new URL(request.url()).pathname,
				method: request.method()
			});
		});
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

	/** Refreshes the page **asked for**, which is what a timer firing produces. */
	get refreshesAsked() {
		return this.sent.filter(
			(entry) => entry.url === '/api/session/refresh' && entry.method === 'POST'
		);
	}

	get successfulRefreshes() {
		return this.refreshes.filter((entry) => entry.status === 200);
	}

	clear() {
		this.seen.length = 0;
		this.sent.length = 0;
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
