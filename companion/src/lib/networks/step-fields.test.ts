// The field-type-driven renderer's decisions, which are the ones that can put a
// credential on screen or spend a user's one-time code.

import { describe, expect, it } from 'vitest';

import type { InputField } from './login-view';
import { answerable, answerOf, controlsOf } from './step-fields';

function field(overrides: Partial<InputField> & { id: string }): InputField {
	return {
		name: overrides.id,
		description: null,
		type: 'username',
		pattern: null,
		cookieDomain: null,
		...overrides
	};
}

describe('the controls a step draws', () => {
	it('gives an ordinary field one control, typed by the bridge’s own type', () => {
		const controls = controlsOf([
			field({ id: 'phone_number', type: 'phone_number' }),
			field({ id: 'password', type: 'password' }),
			field({ id: 'code', type: '2fa_code' })
		]);
		expect(controls).toMatchObject([
			{ kind: 'entry', control: 'tel', secret: false },
			{ kind: 'entry', control: 'password', secret: true },
			// The phone can offer the code out of the message that just arrived,
			// which is the difference between a login finished and one abandoned.
			{ kind: 'entry', control: 'text', autocomplete: 'one-time-code' }
		]);
	});

	it('collects a jar of cookies through one control, in the bridge’s order', () => {
		// People arrive at that step holding a whole `Cookie` header or an
		// extension's export, not seven values to transcribe — and "collect these
		// together" is a signal from the field type, not a special case for one
		// network.
		const controls = controlsOf([
			field({ id: 'SID', type: 'cookie', cookieDomain: '.google.com' }),
			field({ id: 'HSID', type: 'cookie' }),
			field({ id: 'SAPISID', type: 'cookie', cookieDomain: '.google.com' })
		]);
		expect(controls).toHaveLength(1);
		expect(controls[0]).toMatchObject({
			kind: 'jar',
			type: 'cookie',
			domain: '.google.com',
			names: ['SID', 'HSID', 'SAPISID']
		});
	});

	it('puts the jar where the bridge put the first of its fields', () => {
		const controls = controlsOf([
			field({ id: 'email', type: 'email' }),
			field({ id: 'SID', type: 'cookie' }),
			field({ id: 'token', type: 'token' }),
			field({ id: 'HSID', type: 'cookie' })
		]);
		expect(controls.map((control) => control.id)).toEqual(['email', 'jar:cookie', 'token']);
	});

	it('refuses a field whose type the bridge did not declare', () => {
		// The worst case available to a type-driven renderer, and the one this
		// used to produce: a password field drawn as a plain text input because
		// an absent type silently became `username` (#175).
		const controls = controlsOf([field({ id: 'secret', name: 'Secret', type: null })]);
		expect(controls).toMatchObject([{ kind: 'refused', because: 'no_type' }]);
		expect(answerable(controls)).toBe(false);
	});

	it('refuses a field type this build does not know, rather than guessing', () => {
		const controls = controlsOf([field({ id: 'passkey', type: 'fi.mau.future.passkey' })]);
		expect(controls).toMatchObject([{ kind: 'refused', because: 'unknown_type' }]);
		expect(answerable(controls)).toBe(false);
	});

	it('refuses the whole step when one of its fields is refused', () => {
		// Sending part of a step does not get a correction: the network ends the
		// login when it refuses an answer, and the user pays a fresh code for it.
		const controls = controlsOf([
			field({ id: 'phone_number', type: 'phone_number' }),
			field({ id: 'mystery', type: null })
		]);
		expect(answerable(controls)).toBe(false);
	});

	it('has nothing to answer when the bridge listed no fields', () => {
		expect(answerable(controlsOf([]))).toBe(false);
	});
});

describe('the answer a step is submitted with', () => {
	it('puts an ordinary field under its own id, verbatim', () => {
		const controls = controlsOf([field({ id: 'phone_number', type: 'phone_number' })]);
		expect(answerOf(controls, { phone_number: '+33600000000' })).toEqual({
			ok: true,
			data: { phone_number: '+33600000000' }
		});
	});

	it('keeps a credential’s own whitespace', () => {
		// "Cleaning" a value is how a login fails with no visible cause: a token
		// is whatever the network minted, trailing spaces included.
		const controls = controlsOf([field({ id: 'token', type: 'token' })]);
		expect(answerOf(controls, { token: ' a-token-with-space ' })).toEqual({
			ok: true,
			data: { token: ' a-token-with-space ' }
		});
	});

	it('puts a jar under the member its type names', () => {
		const controls = controlsOf([
			field({ id: 'SID', type: 'cookie' }),
			field({ id: 'HSID', type: 'cookie' })
		]);
		expect(answerOf(controls, { 'jar:cookie': 'SID=one; HSID=two' })).toEqual({
			ok: true,
			data: { cookies: { SID: 'one', HSID: 'two' } }
		});
	});

	it('names the cookies a paste is missing rather than saying “invalid”', () => {
		const controls = controlsOf([
			field({ id: 'SID', type: 'cookie' }),
			field({ id: '__Secure-1PSIDTS', type: 'cookie' })
		]);
		const answer = answerOf(controls, { 'jar:cookie': 'SID=one' });
		expect(answer).toEqual({
			ok: false,
			problems: [
				{ controlId: 'jar:cookie', because: 'incomplete', names: ['__Secure-1PSIDTS'] }
			]
		});
	});

	it('says a paste could not be read as cookies at all', () => {
		const controls = controlsOf([field({ id: 'SID', type: 'cookie' })]);
		expect(answerOf(controls, { 'jar:cookie': 'I could not find them' })).toEqual({
			ok: false,
			problems: [{ controlId: 'jar:cookie', because: 'unreadable' }]
		});
	});

	it('reports every empty control at once', () => {
		const controls = controlsOf([
			field({ id: 'phone_number', type: 'phone_number' }),
			field({ id: 'password', type: 'password' })
		]);
		expect(answerOf(controls, { phone_number: '   ' })).toEqual({
			ok: false,
			problems: [
				{ controlId: 'phone_number', because: 'empty' },
				{ controlId: 'password', because: 'empty' }
			]
		});
	});

	it('checks the bridge’s own pattern before the network can end the login', () => {
		const controls = controlsOf([
			field({ id: 'phone_number', type: 'phone_number', pattern: '^\\+[0-9]{8,}$' })
		]);
		expect(answerOf(controls, { phone_number: '+1' })).toMatchObject({ ok: false });
		expect(answerOf(controls, { phone_number: '+33600000000' })).toMatchObject({ ok: true });
	});

	it('ignores a pattern this browser cannot compile', () => {
		// An unusable pattern must never be the reason a correct answer is
		// refused: the network decides then, as it would have anyway.
		const controls = controlsOf([field({ id: 'name', type: 'username', pattern: '([' })]);
		expect(answerOf(controls, { name: 'anything' })).toMatchObject({ ok: true });
	});
});
