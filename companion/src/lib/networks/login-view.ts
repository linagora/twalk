// What a bridge login looks like on screen, derived from the one document the
// Companion polls (`BridgeLogin` in `companion-gateway/openapi.yaml`).
//
// Pure, and the reason screens 3a and 3b are the same component: the Gateway
// reports one shape for every bridge, and every state the wireframes draw is a
// function of it. `login-session.ts` does the polling; this file decides what
// the polled document means.
//
// Two rules the Gateway's description is explicit about, and that live here so
// no screen has to remember them:
//
//   - **`generation` is the fact, the clock is an estimate.** A QR step's
//     `valid_for_seconds` is the Gateway's own guess at the network's refresh
//     interval, not a promise; a bumped generation carrying a new payload is
//     what says a fresh code arrived. So the countdown is decoration and
//     [`qrKey`] — the generation — is what a renderer keys off.
//   - **Branch on `error.code`, never on `detail`.** `detail` is an operator's
//     sentence; the codes are the stable contract.

import type { components } from '$lib/api/schema';

export type BridgeLogin = components['schemas']['BridgeLogin'];
export type BridgeLoginStep = components['schemas']['BridgeLoginStep'];
export type LoginErrorCode = NonNullable<BridgeLogin['error']>['code'];

/** A `user_input` step's fields, as bridgev2 describes them. */
export interface InputField {
	readonly id: string;
	readonly name: string;
	readonly description: string | null;
	/** bridgev2's own field types: `phone_number`, `email`, `username`, `password`, `token`. */
	readonly type: string;
	readonly pattern: string | null;
}

/** A `cookies` step: the page to sign in on, and what to bring back. */
export interface CookieRequest {
	readonly url: string | null;
	readonly fields: readonly string[];
	readonly extractJs: string | null;
	readonly waitForUrl: string | null;
}

/**
 * The state screen 3a/3b/3c renders. A closed union: a screen that handles
 * every member handles every state the Gateway can report, and adding one to
 * the Gateway makes the screens fail to compile rather than fall through to a
 * blank page.
 */
export type LoginView =
	| { kind: 'idle' }
	| { kind: 'starting' }
	/** A code to draw. `data` is raw payload — the bridge renders no image. */
	| {
			kind: 'qr';
			data: string;
			generation: number;
			instructions: string | null;
			expiresAt: string;
			validForSeconds: number;
	  }
	/** Scanned, or a blocking step with nothing to draw: "verifying your session…". */
	| { kind: 'verifying'; instructions: string | null }
	| { kind: 'input'; stepId: string; instructions: string | null; fields: readonly InputField[] }
	| { kind: 'cookies'; stepId: string; instructions: string | null; request: CookieRequest }
	| { kind: 'complete'; loginId: string | null; userId: string | null }
	| { kind: 'failed'; code: LoginErrorCode }
	| { kind: 'cancelled' };

/**
 * A second device is already mid-scan. Its own state, not an error banner: the
 * wireframes' one-login-at-a-time refusal is a screen that names the device and
 * the instant, and offers to take the login over.
 */
export interface LoginConflict {
	readonly deviceName: string;
	readonly startedAt: string;
}

/** Everything the QR screens need, in one value. */
export interface LoginState {
	readonly view: LoginView;
	/** Set instead of `view` when the Gateway refused with `login_in_flight`. */
	readonly conflict: LoginConflict | null;
	/** A Gateway or transport refusal the screen shows as a retryable banner. */
	readonly trouble: TroubleCode | null;
}

/**
 * What went wrong outside the login itself. `unauthenticated` sends the user
 * back to sign-in; the rest are "try again".
 */
export type TroubleCode =
	| 'unauthenticated'
	| 'unknown_bridge'
	| 'too_many_logins'
	| 'bridge_unreachable'
	| 'bridge_refused'
	| 'network'
	| 'unexpected';

export const IDLE: LoginState = { view: { kind: 'idle' }, conflict: null, trouble: null };

/** The polled document, read as a screen state. */
export function viewOf(login: BridgeLogin): LoginView {
	switch (login.state) {
		case 'complete':
			return {
				kind: 'complete',
				loginId: login.login?.login_id ?? null,
				userId: login.login?.user_id ?? null
			};
		case 'failed':
			// `error` is required alongside `failed`, but a document that
			// contradicts itself must still render something honest.
			return { kind: 'failed', code: login.error?.code ?? 'bridge_refused' };
		case 'cancelled':
			return { kind: 'cancelled' };
		case 'awaiting_input':
		case 'awaiting_remote':
			return stepView(login.step);
	}
}

function stepView(step: BridgeLoginStep | null): LoginView {
	if (step === null) {
		return { kind: 'starting' };
	}
	switch (step.type) {
		case 'display_and_wait': {
			const data = qrPayload(step);
			if (data === null) {
				// A blocking step with nothing to draw: the code was scanned and
				// the network is deciding. The wireframes' "Verifying your
				// session…".
				return { kind: 'verifying', instructions: step.instructions };
			}
			return {
				kind: 'qr',
				data,
				generation: 0, // filled in by `stateOf`, which has the login
				instructions: step.instructions,
				expiresAt: step.expires_at,
				validForSeconds: step.valid_for_seconds
			};
		}
		case 'user_input':
			return {
				kind: 'input',
				stepId: step.step_id,
				instructions: step.instructions,
				fields: inputFields(step)
			};
		case 'cookies':
			return {
				kind: 'cookies',
				stepId: step.step_id,
				instructions: step.instructions,
				request: cookieRequest(step)
			};
		case 'complete':
			return { kind: 'verifying', instructions: step.instructions };
		// The Gateway ends the login rather than reporting either of these
		// (`error.code` is `webauthn_required` / `unsupported_step`), so
		// reaching here means a Gateway newer than this build.
		case 'webauthn':
			return { kind: 'failed', code: 'webauthn_required' };
		case 'client_http':
			return { kind: 'failed', code: 'unsupported_step' };
	}
}

/** The polled document as the whole screen state, generation included. */
export function stateOf(login: BridgeLogin): LoginState {
	const view = viewOf(login);
	return {
		view: view.kind === 'qr' ? { ...view, generation: login.generation } : view,
		conflict: null,
		trouble: null
	};
}

/**
 * The raw payload of a QR step, or `null` when the step carries none.
 *
 * bridgev2 answers `{"type": "qr", "data": "…"}`; a bridge that ever answered
 * an image URL instead would be answering something this Companion cannot
 * draw, and `null` — "verifying" — is a better answer than a broken image.
 */
export function qrPayload(step: BridgeLoginStep): string | null {
	const payload = step.payload;
	if (payload === null || typeof payload !== 'object') {
		return null;
	}
	const type = payload['type'];
	if (type !== undefined && type !== 'qr') {
		return null;
	}
	const data = payload['data'];
	return typeof data === 'string' && data.length > 0 ? data : null;
}

function inputFields(step: BridgeLoginStep): InputField[] {
	const raw = step.payload?.['fields'];
	if (!Array.isArray(raw)) {
		return [];
	}
	return raw.flatMap((entry): InputField[] => {
		if (entry === null || typeof entry !== 'object') {
			return [];
		}
		const field = entry as Record<string, unknown>;
		const id = typeof field['id'] === 'string' ? field['id'] : null;
		if (id === null) {
			return [];
		}
		return [
			{
				id,
				name: typeof field['name'] === 'string' ? field['name'] : id,
				description: typeof field['description'] === 'string' ? field['description'] : null,
				type: typeof field['type'] === 'string' ? field['type'] : 'username',
				pattern: typeof field['pattern'] === 'string' ? field['pattern'] : null
			}
		];
	});
}

function cookieRequest(step: BridgeLoginStep): CookieRequest {
	const payload = step.payload ?? {};
	const fields = payload['fields'];
	const names: string[] = Array.isArray(fields)
		? fields.flatMap((entry) => {
				if (typeof entry === 'string') {
					return [entry];
				}
				if (entry !== null && typeof entry === 'object') {
					const id = (entry as Record<string, unknown>)['id'];
					if (typeof id === 'string') {
						return [id];
					}
				}
				return [];
			})
		: [];
	return {
		url: typeof payload['url'] === 'string' ? payload['url'] : null,
		fields: names,
		extractJs: typeof payload['extract_js'] === 'string' ? payload['extract_js'] : null,
		waitForUrl: typeof payload['wait_for_url'] === 'string' ? payload['wait_for_url'] : null
	};
}

/**
 * The Gateway's `409 login_in_flight`, read back into the two facts the screen
 * shows: *which of my devices*, and *when*.
 *
 * The `detail` sentence is the only place those two live — the refusal has no
 * structured members — so this is the one place the Companion reads a `detail`,
 * and it degrades to a screen that still names the refusal when the wording
 * moves. It never *branches* on it: `error === 'login_in_flight'` decided that
 * already.
 */
export function conflictFrom(detail: string | null | undefined): LoginConflict {
	const startedAt = /started at (\S+)/.exec(detail ?? '')?.[1] ?? '';
	const deviceName = /device "([^"]*)"/.exec(detail ?? '')?.[1] ?? '';
	return { deviceName, startedAt };
}

/** Seconds left on a step, floored at zero. `now` is passed in, so this is pure. */
export function secondsLeft(expiresAt: string, now: number): number {
	const deadline = Date.parse(expiresAt);
	if (Number.isNaN(deadline)) {
		return 0;
	}
	return Math.max(0, Math.round((deadline - now) / 1000));
}
