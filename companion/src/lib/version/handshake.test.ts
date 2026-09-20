// The comparison the version handshake turns on. Playwright drives the whole
// thing against a served build; this is the part where getting it wrong means
// a reload loop rather than a wrong pixel.

import { describe, expect, it } from 'vitest';

import { compareBuilds, compareVersions } from './handshake';

describe('compareVersions', () => {
	it('matches when the Gateway reports the version this build was made for', () => {
		expect(compareVersions('0.1.0', '0.1.0')).toEqual({ kind: 'match', version: '0.1.0' });
	});

	it('reports a mismatch in both directions', () => {
		// An older Gateway is as much a mismatch as a newer one: the shell was
		// generated against a description this binary does not implement.
		expect(compareVersions('0.1.0', '0.2.0')).toEqual({
			kind: 'mismatch',
			expected: '0.1.0',
			actual: '0.2.0'
		});
		expect(compareVersions('0.2.0', '0.1.0')).toEqual({
			kind: 'mismatch',
			expected: '0.2.0',
			actual: '0.1.0'
		});
	});

	it('calls a versionless answer unreachable, never a mismatch', () => {
		// A proxy's own error page, or a 502 with a JSON body that happens to
		// parse: treating that as a mismatch would reload the app in a loop.
		expect(compareVersions('0.1.0', undefined).kind).toBe('unreachable');
		expect(compareVersions('0.1.0', null).kind).toBe('unreachable');
		expect(compareVersions('0.1.0', '').kind).toBe('unreachable');
	});
});

describe('compareBuilds', () => {
	const match = { kind: 'match', version: '0.1.0' } as const;

	it('is a stale shell when the Gateway ships a build other than the one running', () => {
		// The case #222 was filed on: the Companion redeployed, the Gateway the
		// same version, and an open tab still running the previous build.
		expect(compareBuilds('1789839442194', '1789900000000', match)).toEqual({
			kind: 'stale-shell',
			running: '1789839442194',
			shipped: '1789900000000'
		});
	});

	it('leaves the first comparison alone when the builds agree', () => {
		expect(compareBuilds('1789839442194', '1789839442194', match)).toBe(match);
	});

	it('never calls a shell stale on a Gateway that names no build', () => {
		// An export without `_app/version.json`, or a Gateway older than the
		// field: not comparable, and a reload on a guess would be a loop.
		expect(compareBuilds('1789839442194', null, match)).toBe(match);
		expect(compareBuilds('1789839442194', undefined, match)).toBe(match);
		expect(compareBuilds('1789839442194', '', match)).toBe(match);
	});
});

