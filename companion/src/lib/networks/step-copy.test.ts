// This project's words for a bridge's step: which step they are for, and what
// happens when the step stops being the one they were written for.
//
// The second half is the interesting one. An explanation keyed on a step id can
// rot — a bridge that changes what a step asks for takes the explanation's truth
// with it — and the mitigation ADR 0030 chose is to fail loudly rather than read
// a stale explanation out with confidence. So these tests are mostly about
// drift.

import { describe, expect, it } from 'vitest';

import { SMS, WHATSAPP } from './copy';
import type { InputField, LoginView } from './login-view';
import { driftOf, overrideFor } from './step-copy';

function field(id: string, type: string | null): InputField {
	return { id, name: id, description: null, type, pattern: null, cookieDomain: null };
}

function cookies(fields: readonly InputField[]): LoginView {
	return {
		kind: 'cookies',
		stepId: 'fi.mau.stub.login.cookies',
		instructions: 'Paste the cookies from a private window',
		fields,
		request: { url: 'https://messages.google.com/web/authentication', extractJs: null, waitForUrl: null }
	};
}

const JAR = [field('SID', 'cookie'), field('HSID', 'cookie')];

describe('the step a screen has words for', () => {
	it('is found by the bridge’s own step id', () => {
		expect(overrideFor(SMS.stepCopy, 'fi.mau.stub.login.cookies')).not.toBeNull();
		expect(overrideFor(SMS.stepCopy, 'fi.mau.gmessages.login.cookies')).not.toBeNull();
	});

	it('is absent for a step nobody wrote about', () => {
		// Which is a screen that shows the bridge's own words and says an
		// explanation is missing — never a step drawn with somebody else's.
		expect(overrideFor(SMS.stepCopy, 'fi.mau.gmessages.login.renamed')).toBeNull();
		expect(overrideFor(WHATSAPP.stepCopy, 'fi.mau.whatsapp.login.phone')).toBeNull();
	});
});

describe('an explanation against the step in front of it', () => {
	const override = overrideFor(SMS.stepCopy, 'fi.mau.stub.login.cookies');

	it('fits the shape it was written for', () => {
		expect(override).not.toBeNull();
		expect(driftOf(override!, cookies(JAR))).toBeNull();
	});

	it('does not care how many fields of that type there are', () => {
		// Google's jar was six cookies and is seven; the explanation is about
		// cookies, and the count is the bridge's business.
		expect(driftOf(override!, cookies([...JAR, field('SAPISID', 'cookie')]))).toBeNull();
	});

	it('has drifted when the step asks for another type', () => {
		// The day this step asks for a password is not a day to read the user
		// three paragraphs about private windows.
		expect(driftOf(override!, cookies([field('password', 'password')]))).toEqual({
			because: 'field_types',
			expected: 'cookie',
			found: 'password'
		});
	});

	it('has drifted when a field arrives with no type at all', () => {
		expect(driftOf(override!, cookies([field('SID', 'cookie'), field('mystery', null)]))).toEqual({
			because: 'field_types',
			expected: 'cookie',
			found: '(none), cookie'
		});
	});

	it('has drifted when the step is no longer that kind of step', () => {
		const asInput: LoginView = {
			kind: 'input',
			stepId: 'fi.mau.stub.login.cookies',
			instructions: null,
			fields: JAR
		};
		expect(driftOf(override!, asInput)).toEqual({
			because: 'kind',
			expected: 'cookies',
			found: 'input'
		});
	});

	it('has drifted when the login is not on a step with fields at all', () => {
		expect(driftOf(override!, { kind: 'cancelled' })).toMatchObject({ because: 'kind' });
	});
});

describe('the SMS cookie step’s words', () => {
	// #57's four requirements are acceptance criteria, and the migration onto the
	// shared renderer is exactly where they could have been dropped in silence.
	// This asserts they are still attached to the step that needs them.
	const override = overrideFor(SMS.stepCopy, 'fi.mau.stub.login.cookies');
	const keys = [
		...(override?.before ?? []).flatMap((block) => [
			block.title,
			block.body,
			...block.steps,
			...block.bullets
		]),
		...(override?.after ?? []),
		override?.refused ?? null
	];

	it('say why a private window is required and that DBSC must be off', () => {
		expect(keys).toContain('sms.step2.privateWindow');
		expect(keys).toContain('sms.step2.dbsc');
	});

	it('say where to get the cookies, and what they let their holder do', () => {
		expect(keys).toContain('sms.step2.a');
		expect(keys).toContain('sms.cookies.whatTheyAre');
	});

	it('replace the bridge’s instructions rather than sitting above them', () => {
		expect(override?.instructions).toBeNull();
	});

	it('carry this step’s own failure copy, which the generic sentence cannot', () => {
		expect(override?.refused).toBe('sms.failed.google');
	});
});
