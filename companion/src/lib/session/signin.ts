// Signing this browser in to the Companion Gateway, once the user has a Matrix
// session.
//
// ADR 0011: the Gateway holds no Matrix access token. So the Companion asks the
// homeserver for an **OpenID token** — a credential whose whole power is to
// prove "I am `@you:example.com`" to a third party — and hands that to
// `POST /api/session`. The Gateway verifies it at the homeserver's federation
// `openid/userinfo` endpoint, checks the identity against the one owner it
// serves, and sets its own `HttpOnly` cookies.
//
// Which is why nothing here reads or writes a cookie: the browser attaches
// `twalk_device` itself, and JavaScript cannot see it. The Matrix access token
// stays in memory (`$lib/onboarding/progress.ts`) and is never persisted — spec #65
// keeps nothing sensitive in browser storage, and a Gateway session that
// outlives the page is what "stay signed in" is made of.

import { gateway } from '$lib/api/client';
import { sessionIssued } from './refresh';
import { noteLive } from './state';

/** Why signing in to the Gateway did not work. */
export type SignInProblem =
	/** The homeserver would not mint an OpenID token for this session. */
	| 'openid-refused'
	/** The identity is not this deployment's owner (ADR 0011). */
	| 'not-the-owner'
	/** `GATEWAY_OWNER` is unset: nobody can sign in here. */
	| 'not-configured'
	/** The Gateway could not verify the token at the homeserver. */
	| 'homeserver-unverifiable'
	| 'failed';

export class SignInError extends Error {
	constructor(
		readonly problem: SignInProblem,
		readonly detail: string
	) {
		super(`${problem}: ${detail}`);
		this.name = 'SignInError';
	}
}

export interface SignedIn {
	owner: string;
	homeserver: string;
	deviceId: string;
	/** Seconds until the device token expires. */
	expiresIn: number;
}

/**
 * Mints an OpenID token at the homeserver and exchanges it for a Gateway
 * session.
 *
 * `deviceName` is what the user will see in their device list; the wireframes
 * let them rename it later, and the Gateway names an unnamed device itself.
 */
export async function signInToGateway(options: {
	baseUrl: string;
	userId: string;
	accessToken: string;
	deviceName?: string;
}): Promise<SignedIn> {
	const token = await requestOpenIdToken(options.baseUrl, options.userId, options.accessToken);

	const result = await gateway.POST('/api/session', {
		body: {
			matrix_openid_token: token,
			...(options.deviceName === undefined ? {} : { device_name: options.deviceName })
		}
	});

	if (result.data !== undefined) {
		// The session starts being kept alive here, with the lifetime the
		// Gateway just named: `expires_in` exists for exactly this, and until
		// #111 nothing read it (`$lib/session/refresh.ts`).
		noteLive(result.data);
		sessionIssued(result.data);
		return {
			owner: result.data.owner,
			homeserver: result.data.homeserver,
			deviceId: result.data.device.id,
			expiresIn: result.data.expires_in
		};
	}

	const status = result.response.status;
	const code = (result.error as { error?: string } | undefined)?.error ?? '';
	if (status === 403 || code === 'not_the_owner') {
		throw new SignInError('not-the-owner', 'this deployment serves a different owner');
	}
	if (status === 503) {
		throw new SignInError('not-configured', 'this deployment has no owner configured');
	}
	if (status === 502 || code === 'homeserver_unverifiable') {
		throw new SignInError(
			'homeserver-unverifiable',
			'the Gateway could not reach the homeserver to verify the token'
		);
	}
	throw new SignInError('failed', `the Gateway answered ${status}${code === '' ? '' : ` (${code})`}`);
}

/**
 * `POST /_matrix/client/v3/user/{userId}/openid/request_token`. The document
 * is forwarded to the Gateway **unchanged**, as the description requires: the
 * Gateway reads `access_token` and `matrix_server_name` and ignores the rest,
 * so a client never has to know which members matter.
 */
async function requestOpenIdToken(
	baseUrl: string,
	userId: string,
	accessToken: string
): Promise<{ access_token: string } & Record<string, unknown>> {
	let response: Response;
	try {
		response = await fetch(
			`${baseUrl}/_matrix/client/v3/user/${encodeURIComponent(userId)}/openid/request_token`,
			{
				method: 'POST',
				credentials: 'omit',
				headers: {
					'content-type': 'application/json',
					authorization: `Bearer ${accessToken}`
				},
				body: '{}'
			}
		);
	} catch (cause) {
		throw new SignInError(
			'openid-refused',
			cause instanceof Error ? cause.message : 'the homeserver did not answer'
		);
	}
	if (!response.ok) {
		throw new SignInError('openid-refused', `the homeserver answered ${response.status}`);
	}
	const token: unknown = await response.json().catch(() => null);
	if (
		token === null ||
		typeof token !== 'object' ||
		typeof (token as { access_token?: unknown }).access_token !== 'string'
	) {
		throw new SignInError('openid-refused', 'the homeserver answered no OpenID token');
	}
	// Forwarded whole, as the description requires: the Gateway reads what it
	// needs and ignores the rest, so this client never has to know which
	// members matter.
	return token as { access_token: string } & Record<string, unknown>;
}
