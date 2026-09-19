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
//     sentence; the codes are the stable contract. Two places *show* a
//     `detail` without branching on it — the concurrent-login refusal, whose
//     two facts live nowhere else, and a refused answer, which is the
//     network's own reason — and both degrade to a screen that still says
//     what happened when the wording moves.

import type { components } from '$lib/api/schema';

export type BridgeLogin = components['schemas']['BridgeLogin'];
export type BridgeLoginStep = components['schemas']['BridgeLoginStep'];
export type LoginErrorCode = NonNullable<BridgeLogin['error']>['code'];

/**
 * One field a step asks for, from either of the two shapes bridgev2 declares.
 *
 * The two are not the same document, which is the thing to know before reading
 * the parse below (`mautrix/go`, `bridgev2/login.go`):
 *
 *   - a `user_input` step's field is a `LoginInputDataField`, whose `type` sits
 *     on the field itself;
 *   - a `cookies` step's field is a `LoginCookieField`, which has **no type**:
 *     it carries `sources`, a *list* of `LoginCookieFieldSource`, each with its
 *     own `type`, the `name` the value goes by in the browser, and a
 *     `cookie_domain`. It also carries `required`.
 *
 * Reading only the field's own `type` therefore refuses every real bridgev2
 * cookie step as "the bridge declared no type" — the failure mode this file's
 * `null` was introduced to make *visible*, arrived at from the other side.
 */
export interface InputField {
	/** What the answer is submitted under. Never what is shown to the user. */
	readonly id: string;
	readonly name: string;
	readonly description: string | null;
	/**
	 * bridgev2's own field type, and `null` when the bridge declared none.
	 *
	 * For a `user_input` field it is `type`; for a `cookies` field it is the
	 * first source's `type`, since that is where a cookie field's type lives.
	 * The set of values this Companion draws is `step-fields.ts`'s, taken from
	 * bridgev2's own enumerations rather than from what this repository has
	 * happened to meet.
	 *
	 * `null` rather than a default, because the type is what decides how the
	 * field is drawn (ADR 0030): this used to become `username`, so a password
	 * field whose type a bridge had not declared would have been drawn as a
	 * plain text input with its value on screen. An absent type is refused and
	 * named, exactly as an unknown one is.
	 */
	readonly type: string | null;
	readonly pattern: string | null;
	/**
	 * What the value is called **where the user will find it** — a cookie's or
	 * a header's own name (`sources[].name`), which is not always the `id` the
	 * answer is submitted under.
	 *
	 * `null` when the bridge named no source, and then the id is the best guess
	 * available.
	 */
	readonly sourceName: string | null;
	/**
	 * The domain a grouped field's source names, when it names one.
	 *
	 * Read because it is what a jar can say about itself: the bridge stating
	 * that these values belong to one origin is worth putting on screen.
	 */
	readonly cookieDomain: string | null;
	/**
	 * Whether the bridge says the login needs this one — `LoginCookieField`'s
	 * `required`, and `true` for every `user_input` field, which has no such
	 * member.
	 *
	 * It decides whether a field this build cannot collect stops the step:
	 * refusing a *required* field means the step cannot be answered, and
	 * refusing an optional one must not.
	 */
	readonly required: boolean;
	/** The choices of a `select` field (`LoginInputDataField.options`). */
	readonly options: readonly string[];
}

/**
 * A `cookies` step's own members, beside its fields: where to sign in, and the
 * two extraction hints a browser extension would use.
 *
 * What to bring back is **not** here: it is the step's `fields`, read by the
 * same parser a `user_input` step's are, because "collect these together" is a
 * signal from the field type and not a special case for one network
 * (ADR 0030).
 */
export interface CookieRequest {
	readonly url: string | null;
	readonly extractJs: string | null;
	/** bridgev2's `wait_for_url_pattern`; a regular expression, not a URL. */
	readonly waitForUrl: string | null;
}

/**
 * The state one bridge login is in, as a screen draws it.
 *
 * A closed union, and the discriminant a **dispatch table** is typed over
 * (`login-panels.ts`): a kind added here and drawn by nobody is a missing
 * property in that table, which is a compile error.
 *
 * This comment used to claim that adding a state to the Gateway would make the
 * screens fail to compile rather than fall through to a blank page. That was
 * false and expensive. A Svelte `{#if}` chain receives no exhaustiveness check
 * of any kind, `QrLogin.svelte` had no `{:else}`, and an `input`, `cookies` or
 * `emoji` step therefore drew an empty seventeen-rem box that the session
 * polled once a second for ever — reached in production by any Telegram QR
 * login on an account with two-factor authentication. A comment claiming a
 * guarantee the language does not give is worse than no comment: the table is
 * what makes the sentence true, and [`unknown_step`] is what happens when the
 * *Gateway* is newer than this build, which no type can prevent.
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
	/**
	 * An emoji to match on the phone: the SMS preview path's pairing step,
	 * which is the same blocking step with a different payload type. Shown
	 * large; there is nothing to submit, only to confirm on the device.
	 */
	| {
			kind: 'emoji';
			emoji: string;
			generation: number;
			instructions: string | null;
			expiresAt: string;
			validForSeconds: number;
	  }
	/** Scanned, or a blocking step with nothing to draw: "verifying your session…". */
	| { kind: 'verifying'; instructions: string | null }
	/** A question with fields to answer. The fields' types decide the controls. */
	| { kind: 'input'; stepId: string; instructions: string | null; fields: readonly InputField[] }
	/**
	 * A jar of cookies to bring back from another origin — the same question
	 * with fields of one grouped type, plus the page to fetch them from.
	 */
	| {
			kind: 'cookies';
			stepId: string;
			instructions: string | null;
			fields: readonly InputField[];
			request: CookieRequest;
	  }
	| { kind: 'complete'; loginId: string | null; userId: string | null }
	| { kind: 'failed'; code: LoginErrorCode }
	/**
	 * The network would not accept the answer. Its own outcome and not a
	 * banner over the step, because the bridge **drops the login process** when
	 * it refuses a value: there is nothing left to resubmit to, so the step
	 * goes and the only way on is a fresh login (ADR 0030).
	 *
	 * `detail` is the Gateway's own sentence, which names the network's code
	 * and says the process is gone. Shown, never branched on.
	 */
	| { kind: 'refused'; stepId: string; detail: string | null }
	/**
	 * A step type this build has no panel for: a Gateway newer than this
	 * Companion. The residual case the dispatch table cannot remove, because
	 * the wire is not the type system.
	 */
	| { kind: 'unknown_step'; stepId: string; stepType: string; instructions: string | null }
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
	/**
	 * The Gateway would not take the request (`invalid_request`). On the submit
	 * path this is the network refusing a value and becomes the `refused`
	 * *view*; everywhere else it is this Companion sending something the
	 * Gateway will not parse, which is a defect here and not on the server.
	 *
	 * It is in this union because its absence was the bug: the code fell to
	 * `unexpected`, and the user was told "something went wrong on your Twalk
	 * server" when nothing had.
	 */
	| 'refused'
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
			const emoji = displayPayload(step, 'emoji');
			if (emoji !== null) {
				return {
					kind: 'emoji',
					emoji,
					generation: 0, // filled in by `stateOf`, which has the login
					instructions: step.instructions,
					expiresAt: step.expires_at,
					validForSeconds: step.valid_for_seconds
				};
			}
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
				fields: inputFields(step, null)
			};
		case 'cookies':
			return {
				kind: 'cookies',
				stepId: step.step_id,
				instructions: step.instructions,
				fields: inputFields(step, 'cookie'),
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
	// Unreachable through the generated type and reachable in fact: `type` is
	// a string on the wire, and a Gateway that learned a seventh step type
	// answers one. The residual panel names it rather than drawing nothing.
	const unknown: string = step.type;
	return {
		kind: 'unknown_step',
		stepId: step.step_id,
		stepType: unknown,
		instructions: step.instructions
	};
}

/**
 * A refused answer, as its own outcome.
 *
 * Built where the refusal arrives (`login-session.ts`) rather than polled: the
 * bridge has already destroyed the login process, so the next poll would answer
 * `no_login_in_flight` and the screen would go blank on the one question the
 * user most needs answered.
 */
export function refusedView(stepId: string, detail: string | null): LoginView {
	return { kind: 'refused', stepId, detail };
}

/** The polled document as the whole screen state, generation included. */
export function stateOf(login: BridgeLogin): LoginState {
	const view = viewOf(login);
	return {
		view:
			view.kind === 'qr' || view.kind === 'emoji'
				? { ...view, generation: login.generation }
				: view,
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
	return displayPayload(step, 'qr');
}

/**
 * The `data` of a `display_and_wait` payload of a given type, or `null`.
 *
 * bridgev2 answers `{"type": "qr" | "emoji" | …, "data": "…"}`. A type this
 * Companion cannot render reads as `null` — "verifying" — which is a better
 * answer than a broken image or a blank box.
 *
 * A payload with no `type` at all is treated as a QR code: that is what every
 * bridge this ticket drives sends, and the alternative is drawing nothing.
 */
function displayPayload(step: BridgeLoginStep, wanted: 'qr' | 'emoji'): string | null {
	const payload = step.payload;
	if (payload === null || typeof payload !== 'object') {
		return null;
	}
	const type = payload['type'];
	if (type === undefined ? wanted !== 'qr' : type !== wanted) {
		return null;
	}
	const data = payload['data'];
	return typeof data === 'string' && data.length > 0 ? data : null;
}

/**
 * A step's field list, from either of the two shapes bridgev2 declares — see
 * [`InputField`], which says why they differ.
 *
 * `bare` is the type a field named as a **bare string** has. bridgev2 declares
 * every field as an object, and a `cookies` step whose payload listed names
 * only would still be naming cookies — the step type is the only thing that
 * payload says about them, which is not the same as a default type for a field
 * that declared one badly. A `user_input` step passes `null`, so a bare string
 * there is not a field at all rather than a field of some guessed type.
 */
function inputFields(step: BridgeLoginStep, bare: string | null): InputField[] {
	const raw = step.payload?.['fields'];
	if (!Array.isArray(raw)) {
		return [];
	}
	return raw.flatMap((entry): InputField[] => {
		if (typeof entry === 'string') {
			return entry !== '' && bare !== null
				? [
						{
							id: entry,
							name: entry,
							description: null,
							type: bare,
							pattern: null,
							sourceName: null,
							cookieDomain: null,
							required: true,
							options: []
						}
					]
				: [];
		}
		if (entry === null || typeof entry !== 'object') {
			return [];
		}
		const field = entry as Record<string, unknown>;
		const id = typeof field['id'] === 'string' ? field['id'] : null;
		if (id === null) {
			return [];
		}
		// The first source, which is where a `cookies` field's type lives.
		// Several are allowed — one value obtainable from a cookie *or* from a
		// request header — and the first is the one the bridge prefers.
		const source = firstSource(field['sources']);
		return [
			{
				id,
				name: typeof field['name'] === 'string' ? field['name'] : (source?.name ?? id),
				description: typeof field['description'] === 'string' ? field['description'] : null,
				// The field's own type, then its first source's. `null`, never a
				// default: see [`InputField.type`].
				type: text(field['type']) ?? source?.type ?? null,
				pattern: typeof field['pattern'] === 'string' ? field['pattern'] : null,
				sourceName: source?.name ?? null,
				// `cookie_domain` on the source, which is where bridgev2 puts it;
				// the field-level spelling is read too, because the stub bridges
				// that stand in for a real one used to invent it there.
				cookieDomain: source?.cookieDomain ?? text(field['cookie_domain']) ?? null,
				// Absent means required: a field a bridge said nothing about is
				// one this Companion must not quietly leave out of the answer.
				required: field['required'] !== false,
				options: Array.isArray(field['options'])
					? field['options'].filter((option): option is string => typeof option === 'string')
					: []
			}
		];
	});
}

/** A non-empty string, or `null`. */
function text(value: unknown): string | null {
	return typeof value === 'string' && value !== '' ? value : null;
}

/**
 * The first of a cookie field's `sources`, read for its type, its browser-side
 * name and its domain.
 */
function firstSource(
	raw: unknown
): { type: string | null; name: string | null; cookieDomain: string | null } | null {
	if (!Array.isArray(raw)) {
		return null;
	}
	for (const entry of raw) {
		if (entry === null || typeof entry !== 'object') {
			continue;
		}
		const source = entry as Record<string, unknown>;
		return {
			type: text(source['type']),
			name: text(source['name']),
			cookieDomain: text(source['cookie_domain'])
		};
	}
	return null;
}

function cookieRequest(step: BridgeLoginStep): CookieRequest {
	const payload = step.payload ?? {};
	return {
		url: typeof payload['url'] === 'string' ? payload['url'] : null,
		extractJs: typeof payload['extract_js'] === 'string' ? payload['extract_js'] : null,
		waitForUrl: text(payload['wait_for_url_pattern']) ?? text(payload['wait_for_url'])
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
