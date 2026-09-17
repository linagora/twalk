// Screen 2's first step: the account, through the Gateway's registration relay
// (`POST /api/bootstrap/account`, ticket #53).
//
// The relay creates this deployment's **one** account with Synapse's
// registration shared secret, which is what lets a personal server give its
// owner an account without turning on public registration. Everything it can
// refuse maps onto a state the wireframe already describes, and this module is
// that map — one place, so no screen invents a message for a code it met once:
//
//   `403 not_the_owner`               → this deployment's account is another
//                                       username; the operator knows which.
//   `409 account_already_exists`      → not an error at all: the deployment is
//                                       already set up, so the user is
//                                       *returning*, and the store-loss
//                                       journey is where they belong.
//   `503 registration_not_configured` → the wireframe's "This Twalk deployment
//                                       does not allow new accounts."
//   `502 homeserver_refused`          → the homeserver's own policy, carried as
//                                       `matrix_errcode`: `M_USER_IN_USE` is
//                                       the wireframe's "username taken",
//                                       `M_PASSWORD_*` is a password policy.
//
// The Gateway also refuses, loudly, any body carrying something
// recovery-key-shaped (`recovery_key_refused`). Meeting that code would mean
// this app had a bug that breaks screen 2's promise, so it is mapped and named
// rather than folded into "something went wrong".

import { gateway } from '$lib/api/client';
import type { MatrixSession } from '$lib/crypto/bootstrap';

export type RegistrationProblem =
	| 'username-taken'
	| 'username-not-the-owner'
	| 'password-refused'
	| 'account-already-exists'
	| 'registration-closed'
	| 'homeserver-unreachable'
	| 'recovery-key-refused'
	| 'invalid-request'
	| 'failed';

export class RegistrationError extends Error {
	constructor(
		readonly problem: RegistrationProblem,
		readonly detail: string,
		/** The homeserver's own `errcode`, where there was one. */
		readonly matrixErrcode?: string
	) {
		super(`${problem}: ${detail}`);
		this.name = 'RegistrationError';
	}
}

/**
 * Creates the account and returns the Matrix session the browser continues in.
 *
 * `baseUrl` is not sent anywhere: the Gateway knows its own homeserver. It is
 * carried through so the caller gets a session it can hand straight to
 * `$lib/crypto/bootstrap.ts`.
 */
export async function createAccount(options: {
	username: string;
	password: string;
	baseUrl: string;
}): Promise<MatrixSession> {
	const result = await gateway.POST('/api/bootstrap/account', {
		// Exactly these two members. The request object is closed on the
		// Gateway's side, and there is deliberately nothing a recovery key
		// could ride in on.
		body: { username: options.username, password: options.password }
	});

	if (result.data !== undefined) {
		return {
			baseUrl: options.baseUrl,
			userId: result.data.user_id,
			deviceId: result.data.device_id,
			accessToken: result.data.access_token
		};
	}

	const status = result.response.status;
	const error = result.error as
		| { error?: string; detail?: string; matrix_errcode?: string }
		| undefined;
	const code = error?.error ?? '';
	const detail = error?.detail ?? `the Gateway answered ${status}`;
	const errcode = error?.matrix_errcode;

	if (code === 'account_already_exists') {
		throw new RegistrationError('account-already-exists', detail);
	}
	if (code === 'not_the_owner') {
		throw new RegistrationError('username-not-the-owner', detail);
	}
	if (code === 'registration_not_configured' || code === 'sign_in_not_configured') {
		throw new RegistrationError('registration-closed', detail);
	}
	if (code === 'recovery_key_refused') {
		throw new RegistrationError('recovery-key-refused', detail);
	}
	if (code === 'homeserver_unreachable') {
		throw new RegistrationError('homeserver-unreachable', detail);
	}
	if (code === 'homeserver_refused') {
		if (errcode === 'M_USER_IN_USE' || errcode === 'M_INVALID_USERNAME') {
			throw new RegistrationError('username-taken', detail, errcode);
		}
		throw new RegistrationError('password-refused', detail, errcode);
	}
	if (code === 'invalid_request') {
		throw new RegistrationError('invalid-request', detail);
	}
	throw new RegistrationError('failed', detail);
}
