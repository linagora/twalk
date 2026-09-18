// Driving one bridge login from the browser: start it, poll it, submit what a
// step asks for, cancel it.
//
// **The browser polls; it never waits.** The Gateway holds the bridge's
// blocking step itself (ticket #55) and `GET /api/bridges/{id}/login` answers
// immediately with whatever the bridge last handed back. A phone that sleeps
// mid-scan therefore loses a poll, not the login — which is the whole reason
// the wireframes' Server-Sent Events are a polling loop here instead.
//
// What this module does *not* do is decide what a polled document means: that
// is `login-view.ts`, which is pure and tested. This one owns the timers, the
// in-flight request and the refusals.
//
// Nothing here persists anything. A QR payload is a network credential in
// flight: it lives in this object's state and in the SVG on screen, and goes
// when the next generation replaces it. There is no `localStorage` write on
// this path, by design (ADR 0011).

import { get, writable, type Readable } from 'svelte/store';

import { gateway } from '$lib/api/client';
import {
	conflictFrom,
	stateOf,
	IDLE,
	type LoginState,
	type TroubleCode
} from './login-view';

/** How often the polled state is read while a login is in flight. */
const POLL_INTERVAL_MS = 1000;

/**
 * The flow to run, preferring the QR one.
 *
 * The bridge names its own flows, so the ids are the bridge's — mautrix-whatsapp
 * and mautrix-signal both call theirs `qr`, but a bridge that called it
 * something else would still be driven by matching on the id's shape rather
 * than on a hard-coded string.
 */
export function pickFlow(
	flows: readonly { id: string }[],
	prefer: 'qr' | 'cookies' | null
): string | null {
	if (flows.length === 0) {
		return null;
	}
	if (prefer !== null) {
		const wanted = flows.find((flow) => flow.id.toLowerCase().includes(prefer));
		if (wanted !== undefined) {
			return wanted.id;
		}
	}
	return flows[0]?.id ?? null;
}

/** A Gateway refusal, read as the banner the screen shows. */
function troubleOf(code: unknown): TroubleCode {
	switch (code) {
		case 'unauthenticated':
		case 'sign_in_not_configured':
			return 'unauthenticated';
		case 'unknown_bridge':
			return 'unknown_bridge';
		case 'too_many_logins':
			return 'too_many_logins';
		case 'bridge_unreachable':
			return 'bridge_unreachable';
		case 'bridge_refused':
			return 'bridge_refused';
		default:
			return 'unexpected';
	}
}

function errorCode(error: unknown): string | null {
	if (error !== null && typeof error === 'object' && 'error' in error) {
		const code = (error as { error: unknown }).error;
		return typeof code === 'string' ? code : null;
	}
	return null;
}

function errorDetail(error: unknown): string | null {
	if (error !== null && typeof error === 'object' && 'detail' in error) {
		const detail = (error as { detail: unknown }).detail;
		return typeof detail === 'string' ? detail : null;
	}
	return null;
}

/**
 * One bridge's login, as a store a screen subscribes to.
 *
 * The lifecycle is the caller's: `start()` from a click or `onMount`, `stop()`
 * from the component's teardown. Nothing starts on construction, so a screen
 * that only wants to *show* the disclosure card first does not begin a login
 * behind the user's back.
 */
export class LoginSession {
	readonly state: Readable<LoginState>;

	#state = writable<LoginState>(IDLE);
	#bridgeId: string;
	#timer: ReturnType<typeof setTimeout> | null = null;
	#stopped = false;
	/** Set once a start succeeded, so a refresh knows to re-run the same flow. */
	#flowId: string | null = null;

	constructor(bridgeId: string) {
		this.#bridgeId = bridgeId;
		this.state = { subscribe: this.#state.subscribe };
	}

	/**
	 * Starts a login and begins polling.
	 *
	 * `loginId` reconnects an existing login instead of creating a second one —
	 * the Gateway's own word for repairing a broken session. `takeOver` cancels
	 * whatever another device left in flight first, which is the button the
	 * concurrent-login screen offers.
	 */
	async start(options: { prefer?: 'qr' | 'cookies'; loginId?: string; takeOver?: boolean } = {}) {
		this.#stopped = false;
		this.#state.set({ view: { kind: 'starting' }, conflict: null, trouble: null });

		if (options.takeOver === true) {
			await this.#cancelQuietly();
		}

		const flows = await gateway.GET('/api/bridges/{bridge_id}/login/flows', {
			params: { path: { bridge_id: this.#bridgeId } }
		});
		if (flows.error !== undefined) {
			this.#trouble(troubleOf(errorCode(flows.error)));
			return;
		}
		const flowId = pickFlow(flows.data.flows, options.prefer ?? 'qr');
		if (flowId === null) {
			this.#trouble('unknown_bridge');
			return;
		}
		this.#flowId = flowId;

		const started = await gateway.POST('/api/bridges/{bridge_id}/login', {
			params: { path: { bridge_id: this.#bridgeId } },
			body: options.loginId === undefined ? { flow_id: flowId } : { flow_id: flowId, login_id: options.loginId }
		});
		if (started.error !== undefined) {
			const code = errorCode(started.error);
			if (code === 'login_in_flight') {
				this.#state.set({
					view: { kind: 'idle' },
					conflict: conflictFrom(errorDetail(started.error)),
					trouble: null
				});
				return;
			}
			this.#trouble(troubleOf(code));
			return;
		}
		this.#state.set(stateOf(started.data));
		this.#schedule();
	}

	/** Answers the step the login is waiting on. */
	async submit(stepId: string, data: Record<string, unknown>) {
		const answered = await gateway.POST('/api/bridges/{bridge_id}/login/submit', {
			params: { path: { bridge_id: this.#bridgeId } },
			body: { step_id: stepId, data }
		});
		if (answered.error !== undefined) {
			this.#trouble(troubleOf(errorCode(answered.error)));
			return;
		}
		this.#state.set(stateOf(answered.data));
		this.#schedule();
	}

	/** Cancels the login in flight and stops polling. */
	async cancel() {
		this.#clearTimer();
		await this.#cancelQuietly();
		// The Gateway keeps reporting the login as `cancelled`, which is the
		// state the screen draws — so read it rather than inventing one.
		await this.#poll();
		this.#clearTimer();
	}

	/** Stops polling. Call it from the screen's teardown; leaves the login alone. */
	stop() {
		this.#stopped = true;
		this.#clearTimer();
	}

	async #cancelQuietly() {
		await gateway.DELETE('/api/bridges/{bridge_id}/login', {
			params: { path: { bridge_id: this.#bridgeId } }
		});
	}

	#trouble(code: TroubleCode) {
		this.#clearTimer();
		this.#state.set({ view: get(this.#state).view, conflict: null, trouble: code });
	}

	#clearTimer() {
		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
	}

	#schedule() {
		this.#clearTimer();
		if (this.#stopped || !this.#pollable()) {
			return;
		}
		this.#timer = setTimeout(() => {
			void this.#poll().then(() => this.#schedule());
		}, POLL_INTERVAL_MS);
	}

	/** Only a login that can still change is worth polling. */
	#pollable(): boolean {
		const kind = get(this.#state).view.kind;
		return (
			kind === 'qr' ||
			kind === 'emoji' ||
			kind === 'verifying' ||
			kind === 'starting' ||
			kind === 'input' ||
			kind === 'cookies'
		);
	}

	async #poll() {
		if (this.#stopped) {
			return;
		}
		let polled;
		try {
			polled = await gateway.GET('/api/bridges/{bridge_id}/login', {
				params: { path: { bridge_id: this.#bridgeId } }
			});
		} catch {
			// A dropped request is not a failed login: the Gateway holds the
			// step, so the next poll picks it up where this one left off.
			this.#state.update((current) => ({ ...current, trouble: 'network' }));
			return;
		}
		if (this.#stopped) {
			return;
		}
		if (polled.error !== undefined) {
			const code = errorCode(polled.error);
			if (code === 'no_login_in_flight') {
				this.#state.set(IDLE);
				return;
			}
			this.#state.update((current) => ({ ...current, trouble: troubleOf(code) }));
			return;
		}
		this.#state.set(stateOf(polled.data));
	}
}
