// Signing in to a Matrix account the user already has, from the browser.
//
// Screen 3d is the bring-your-own-account path (ADR 0009): the user's account
// exists, possibly on a homeserver that is not the Twalk one, and Twalk's job
// is to be invited into the rooms they choose — not to take their identity
// over. So the session is made **here**, with the homeserver, and the access
// token never leaves this page except as the parameter of the one call that
// needs it (`POST /api/bootstrap/rooms`, ADR 0011).
//
// Two of the wireframe's three methods are implemented:
//
//   - **SSO**, because a homeserver that has it usually has *only* it, and a
//     password form on such a deployment is a dead end. It is the standard
//     redirect dance: the homeserver sends the browser back with a
//     `loginToken`, which is exchanged for a session by `m.login.token`.
//   - **password**, for a homeserver that offers it.
//
// The third, MSC4108 sign-in by QR from another client, needs the crypto stack
// to carry the secrets across and is not here; the screen says so rather than
// showing a control that does nothing.
//
// Nothing in this module writes to storage. The `loginToken` is single-use and
// is removed from the URL as soon as it is spent, so a copied address bar
// carries no credential.

import type { MatrixSession } from '$lib/crypto/bootstrap';

/** What a homeserver says it accepts. */
export interface LoginFlows {
	readonly password: boolean;
	readonly sso: boolean;
	/** Every flow type the homeserver listed, for the screen to be honest about. */
	readonly types: readonly string[];
}

export type LoginProblem =
	/** The homeserver could not be reached at all. */
	| 'unreachable'
	/** Wrong user or password, or a login token that had already been spent. */
	| 'rejected'
	/** The homeserver rate-limited the attempt. */
	| 'rate-limited'
	/** Anything else the homeserver said. */
	| 'refused';

export class MatrixLoginError extends Error {
	constructor(
		readonly problem: LoginProblem,
		readonly detail: string
	) {
		super(`${problem}: ${detail}`);
		this.name = 'MatrixLoginError';
	}
}

/** The flow types, read out of `GET /_matrix/client/v3/login`. */
export function readFlows(document: unknown): LoginFlows {
	const flows =
		document !== null && typeof document === 'object' && Array.isArray((document as { flows?: unknown }).flows)
			? ((document as { flows: unknown[] }).flows ?? [])
			: [];
	const types = flows.flatMap((flow) => {
		if (flow !== null && typeof flow === 'object') {
			const type = (flow as Record<string, unknown>)['type'];
			if (typeof type === 'string') {
				return [type];
			}
		}
		return [];
	});
	return {
		password: types.includes('m.login.password'),
		sso: types.includes('m.login.sso'),
		types
	};
}

export async function loginFlows(baseUrl: string, fetchImpl?: typeof fetch): Promise<LoginFlows> {
	const doFetch = fetchImpl ?? globalThis.fetch.bind(globalThis);
	let response: Response;
	try {
		response = await doFetch(`${baseUrl}/_matrix/client/v3/login`);
	} catch (error) {
		throw new MatrixLoginError('unreachable', String(error));
	}
	if (!response.ok) {
		throw new MatrixLoginError('refused', `the homeserver answered ${response.status}`);
	}
	return readFlows(await response.json());
}

/**
 * Where to send the browser for SSO.
 *
 * `redirectUrl` is this screen's own address, which the homeserver appends a
 * `loginToken` to. It is sent without a query of its own so that the
 * homeserver's append is unambiguous.
 */
export function ssoRedirectUrl(baseUrl: string, redirectUrl: string): string {
	return `${baseUrl}/_matrix/client/v3/login/sso/redirect?redirectUrl=${encodeURIComponent(redirectUrl)}`;
}

/** The `loginToken` the homeserver sent the browser back with, if any. */
export function loginTokenFrom(url: URL): string | null {
	const token = url.searchParams.get('loginToken');
	return token !== null && token.length > 0 ? token : null;
}

async function login(baseUrl: string, body: unknown, fetchImpl?: typeof fetch): Promise<MatrixSession> {
	const doFetch = fetchImpl ?? globalThis.fetch.bind(globalThis);
	let response: Response;
	try {
		response = await doFetch(`${baseUrl}/_matrix/client/v3/login`, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(body)
		});
	} catch (error) {
		throw new MatrixLoginError('unreachable', String(error));
	}
	const answer: unknown = await response.json().catch(() => null);
	if (!response.ok) {
		throw new MatrixLoginError(problemFor(response.status, answer), errcodeOf(answer) ?? `${response.status}`);
	}
	const session = answer as Partial<MatrixSession> & { access_token?: string; user_id?: string; device_id?: string };
	if (typeof session.access_token !== 'string' || typeof session.user_id !== 'string') {
		throw new MatrixLoginError('refused', 'the homeserver returned no session');
	}
	return {
		baseUrl,
		userId: session.user_id,
		deviceId: typeof session.device_id === 'string' ? session.device_id : '',
		accessToken: session.access_token
	};
}

/** `m.login.password`. The password is sent to the homeserver and to nothing else. */
export function loginWithPassword(
	baseUrl: string,
	user: string,
	password: string,
	fetchImpl?: typeof fetch
): Promise<MatrixSession> {
	return login(
		baseUrl,
		{
			type: 'm.login.password',
			identifier: { type: 'm.id.user', user },
			password,
			initial_device_display_name: 'Twalk Companion'
		},
		fetchImpl
	);
}

/** `m.login.token`, the second half of the SSO redirect. */
export function loginWithToken(
	baseUrl: string,
	token: string,
	fetchImpl?: typeof fetch
): Promise<MatrixSession> {
	return login(
		baseUrl,
		{
			type: 'm.login.token',
			token,
			initial_device_display_name: 'Twalk Companion'
		},
		fetchImpl
	);
}

export function problemFor(status: number, answer: unknown): LoginProblem {
	if (status === 429 || errcodeOf(answer) === 'M_LIMIT_EXCEEDED') {
		return 'rate-limited';
	}
	if (status === 401 || status === 403) {
		return 'rejected';
	}
	return 'refused';
}

function errcodeOf(answer: unknown): string | null {
	if (answer !== null && typeof answer === 'object') {
		const errcode = (answer as Record<string, unknown>)['errcode'];
		if (typeof errcode === 'string') {
			return errcode;
		}
	}
	return null;
}
