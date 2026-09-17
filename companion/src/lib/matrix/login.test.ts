// Which sign-in methods a homeserver offers, and the SSO round trip's two
// pure halves. The network calls are Playwright's; these are the decisions
// that would otherwise be made in a component.

import { describe, expect, it } from 'vitest';

import { loginTokenFrom, problemFor, readFlows, ssoRedirectUrl } from './login';

describe('the homeserver’s login flows', () => {
	it('recognises password and SSO', () => {
		const flows = readFlows({
			flows: [{ type: 'm.login.sso' }, { type: 'm.login.token' }, { type: 'm.login.password' }]
		});
		expect(flows.password).toBe(true);
		expect(flows.sso).toBe(true);
		expect(flows.types).toContain('m.login.token');
	});

	it('reports an SSO-only homeserver as such', () => {
		// The case that matters: a password form there is a dead end.
		const flows = readFlows({ flows: [{ type: 'm.login.sso' }, { type: 'm.login.token' }] });
		expect(flows.sso).toBe(true);
		expect(flows.password).toBe(false);
	});

	it('survives a document that is not one', () => {
		expect(readFlows(null).types).toEqual([]);
		expect(readFlows({}).types).toEqual([]);
		expect(readFlows({ flows: 'no' }).types).toEqual([]);
		expect(readFlows({ flows: [null, 7, { type: 'm.login.password' }] }).password).toBe(true);
	});
});

describe('the SSO round trip', () => {
	it('sends the homeserver this screen’s own address, encoded', () => {
		expect(
			ssoRedirectUrl('https://matrix.example.com', 'https://twalk.example.com/networks/matrix')
		).toBe(
			'https://matrix.example.com/_matrix/client/v3/login/sso/redirect' +
				'?redirectUrl=https%3A%2F%2Ftwalk.example.com%2Fnetworks%2Fmatrix'
		);
	});

	it('reads the login token the homeserver sent the browser back with', () => {
		expect(
			loginTokenFrom(new URL('https://twalk.example.com/networks/matrix?loginToken=syt_abc'))
		).toBe('syt_abc');
		expect(loginTokenFrom(new URL('https://twalk.example.com/networks/matrix'))).toBeNull();
		expect(loginTokenFrom(new URL('https://twalk.example.com/networks/matrix?loginToken='))).toBeNull();
	});
});

describe('reading a refusal', () => {
	it('separates the three the screen has different words for', () => {
		expect(problemFor(403, { errcode: 'M_FORBIDDEN' })).toBe('rejected');
		expect(problemFor(401, null)).toBe('rejected');
		expect(problemFor(429, { errcode: 'M_LIMIT_EXCEEDED' })).toBe('rate-limited');
		expect(problemFor(200, { errcode: 'M_LIMIT_EXCEEDED' })).toBe('rate-limited');
		expect(problemFor(500, null)).toBe('refused');
	});
});
