// The polled document, read as a screen state. Every branch here is a state
// the wireframes draw, so a Gateway answer this file misreads is a screen the
// user cannot act on.

import { describe, expect, it } from 'vitest';

import {
	conflictFrom,
	qrPayload,
	refusedView,
	secondsLeft,
	stateOf,
	viewOf,
	type BridgeLogin,
	type BridgeLoginStep
} from './login-view';

function step(overrides: Partial<BridgeLoginStep> = {}): BridgeLoginStep {
	return {
		step_id: 'fi.mau.whatsapp.login.qr',
		type: 'display_and_wait',
		instructions: 'Scan this from WhatsApp',
		payload: { type: 'qr', data: '2@a-raw-payload' },
		received_at: '2026-09-18T07:00:00.000Z',
		valid_for_seconds: 20,
		expires_at: '2026-09-18T07:00:20.000Z',
		...overrides
	};
}

function login(overrides: Partial<BridgeLogin> = {}): BridgeLogin {
	return {
		bridge_id: 'mautrix-whatsapp',
		network: 'whatsapp',
		process_id: 'process-1',
		flow_id: 'qr',
		login_id: null,
		state: 'awaiting_remote',
		started_at: '2026-09-18T07:00:00.000Z',
		started_by: { device_id: 'device-1', device_name: 'the phone' },
		expires_at: '2026-09-18T07:30:00.000Z',
		generation: 3,
		step: step(),
		login: null,
		error: null,
		...overrides
	};
}

describe('the login view', () => {
	it('draws the QR step, carrying the generation the redraw keys off', () => {
		const state = stateOf(login());
		expect(state.view).toMatchObject({ kind: 'qr', data: '2@a-raw-payload', generation: 3 });
	});

	it('shows "verifying" for a blocking step with nothing to draw', () => {
		// The code was scanned and the network is deciding: the payload is gone
		// but the step is still the blocking one.
		expect(viewOf(login({ step: step({ payload: null }) }))).toMatchObject({ kind: 'verifying' });
		expect(viewOf(login({ step: step({ type: 'complete', payload: null }) }))).toMatchObject({
			kind: 'verifying'
		});
	});

	it('reads a completed login', () => {
		const state = viewOf(
			login({ state: 'complete', step: null, login: { login_id: '33612345678', user_id: null } })
		);
		expect(state).toEqual({ kind: 'complete', loginId: '33612345678', userId: null });
	});

	it('reads a failure by its code, never by its detail', () => {
		const state = viewOf(
			login({
				state: 'failed',
				step: null,
				error: { code: 'login_lost', detail: 'the bridge stopped answering at 07:01' }
			})
		);
		expect(state).toEqual({ kind: 'failed', code: 'login_lost' });
	});

	it('reads a cancellation', () => {
		expect(viewOf(login({ state: 'cancelled', step: null }))).toEqual({ kind: 'cancelled' });
	});

	it('fails rather than hanging on a step the Companion cannot drive', () => {
		expect(viewOf(login({ step: step({ type: 'webauthn' }) }))).toEqual({
			kind: 'failed',
			code: 'webauthn_required'
		});
		expect(viewOf(login({ step: step({ type: 'client_http' }) }))).toEqual({
			kind: 'failed',
			code: 'unsupported_step'
		});
	});

	it('reads a user_input step as its fields', () => {
		const state = viewOf(
			login({
				state: 'awaiting_input',
				step: step({
					type: 'user_input',
					step_id: 'phone',
					payload: {
						fields: [
							{ id: 'phone_number', name: 'Phone number', type: 'phone_number' },
							{ name: 'a field with no id' }
						]
					}
				})
			})
		);
		expect(state).toMatchObject({
			kind: 'input',
			stepId: 'phone',
			fields: [{ id: 'phone_number', name: 'Phone number', type: 'phone_number' }]
		});
	});

	it('leaves a field the bridge gave no type as having none', () => {
		// The defect this replaces substituted `username`, so a password field
		// whose type a bridge had not declared would have been drawn as a plain
		// text input with the value on screen. `null` is what lets the renderer
		// refuse it and name it instead (ADR 0030).
		const state = viewOf(
			login({
				state: 'awaiting_input',
				step: step({
					type: 'user_input',
					step_id: 'phone',
					payload: { fields: [{ id: 'secret', name: 'Secret' }, { id: 'blank', type: '' }] }
				})
			})
		);
		expect(state).toMatchObject({ kind: 'input', fields: [{ type: null }, { type: null }] });
	});

	it('reads a cookies step as the page to visit and the cookies to bring back', () => {
		const state = viewOf(
			login({
				state: 'awaiting_input',
				step: step({
					type: 'cookies',
					step_id: 'cookies',
					payload: {
						url: 'https://messages.google.com/web/authentication',
						fields: [
							{ type: 'cookie', cookie_domain: '.google.com', id: 'SID' },
							{ type: 'cookie', id: 'SAPISID' },
							// A bare name: not bridgev2's shape, and in a step whose
							// whole payload is cookies it can be nothing else.
							'HSID'
						]
					}
				})
			})
		);
		expect(state).toMatchObject({
			kind: 'cookies',
			request: { url: 'https://messages.google.com/web/authentication' },
			fields: [
				{ id: 'SID', type: 'cookie', cookieDomain: '.google.com' },
				{ id: 'SAPISID', type: 'cookie', cookieDomain: null },
				{ id: 'HSID', type: 'cookie' }
			]
		});
	});

	it('reads a cookie field’s type out of its sources, which is where bridgev2 puts it', () => {
		// A `LoginCookieField` has **no type of its own**: it carries `sources`,
		// each with the type, the name the value goes by in the browser and the
		// domain (`mautrix/go`, `bridgev2/login.go`). Reading only the field's own
		// `type` refused every real bridgev2 cookie step as "the bridge declared
		// none" — the same defect as the old `username` default, from the other
		// side.
		const state = viewOf(
			login({
				state: 'awaiting_input',
				step: step({
					type: 'cookies',
					step_id: 'fi.mau.linkedin.login.cookies',
					payload: {
						url: 'https://www.linkedin.com/login',
						wait_for_url_pattern: '^https://www\\.linkedin\\.com/feed',
						fields: [
							{
								id: 'cookie',
								required: true,
								sources: [{ type: 'request_header', name: 'Cookie' }]
							},
							{
								id: 'csrf',
								required: false,
								sources: [{ type: 'request_header', name: 'Csrf-Token' }]
							}
						]
					}
				})
			})
		);
		expect(state).toMatchObject({
			kind: 'cookies',
			request: { waitForUrl: '^https://www\\.linkedin\\.com/feed' },
			fields: [
				// The id is what the answer is submitted under; the source name is
				// what the user will see in their browser. They differ here.
				{ id: 'cookie', type: 'request_header', sourceName: 'Cookie', required: true },
				{ id: 'csrf', type: 'request_header', sourceName: 'Csrf-Token', required: false }
			]
		});
	});

	it('names a step type it has no panel for rather than drawing nothing', () => {
		// The residual case: a Gateway newer than this build. The generated type
		// says this cannot happen, and the wire is not the type system — which is
		// the whole reason `unknown_step` exists.
		const state = viewOf(
			login({
				state: 'awaiting_input',
				step: step({
					type: 'fi.mau.something.new' as BridgeLoginStep['type'],
					step_id: 'fi.mau.telegram.login.something',
					instructions: 'Do the new thing'
				})
			})
		);
		expect(state).toEqual({
			kind: 'unknown_step',
			stepId: 'fi.mau.telegram.login.something',
			stepType: 'fi.mau.something.new',
			instructions: 'Do the new thing'
		});
	});
});

describe('a refused answer', () => {
	it('is its own outcome, carrying the Gateway’s own sentence', () => {
		// Not a banner over the step: the bridge destroys the login process when
		// it refuses a value, so the step is gone and a resubmit could only 404.
		expect(refusedView('fi.mau.whatsapp.login.phone', 'the network would not accept it')).toEqual({
			kind: 'refused',
			stepId: 'fi.mau.whatsapp.login.phone',
			detail: 'the network would not accept it'
		});
	});

	it('still says what happened when the Gateway said nothing', () => {
		expect(refusedView('step', null)).toEqual({ kind: 'refused', stepId: 'step', detail: null });
	});
});

describe('the QR payload', () => {
	it('is the raw string the bridge handed over', () => {
		expect(qrPayload(step())).toBe('2@a-raw-payload');
	});

	it('is absent when the step carries none, or carries something else', () => {
		expect(qrPayload(step({ payload: null }))).toBeNull();
		expect(qrPayload(step({ payload: {} }))).toBeNull();
		expect(qrPayload(step({ payload: { type: 'qr', data: '' } }))).toBeNull();
		// An image URL is not something this Companion can draw.
		expect(qrPayload(step({ payload: { type: 'image', url: 'https://…' } }))).toBeNull();
	});
});

describe('the concurrent-login refusal', () => {
	it('reads the device and the instant out of the Gateway’s sentence', () => {
		const conflict = conflictFrom(
			'a login started at 2026-09-18T06:59:12.004Z from the device "the laptop in the kitchen" ' +
				'is still in flight on this bridge; cancel it (DELETE /api/bridges/{bridge_id}/login) ' +
				'before starting another. One login per bridge instance in v0.1'
		);
		expect(conflict).toEqual({
			deviceName: 'the laptop in the kitchen',
			startedAt: '2026-09-18T06:59:12.004Z'
		});
	});

	it('still produces a screen when the wording moves', () => {
		// The refusal carries no structured members, so the sentence is all
		// there is — and a changed sentence must degrade, never throw.
		expect(conflictFrom('something else entirely')).toEqual({ deviceName: '', startedAt: '' });
		expect(conflictFrom(null)).toEqual({ deviceName: '', startedAt: '' });
	});
});

describe('the countdown', () => {
	it('counts down to the step’s expiry and stops at zero', () => {
		const now = Date.parse('2026-09-18T07:00:05.000Z');
		expect(secondsLeft('2026-09-18T07:00:20.000Z', now)).toBe(15);
		expect(secondsLeft('2026-09-18T07:00:00.000Z', now)).toBe(0);
		expect(secondsLeft('not a date', now)).toBe(0);
	});
});
