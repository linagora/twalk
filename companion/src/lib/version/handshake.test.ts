// The comparison the version handshake turns on. Playwright drives the whole
// thing against a served build; this is the part where getting it wrong means
// a reload loop rather than a wrong pixel.

import { describe, expect, it } from 'vitest';

import { compareVersions } from './handshake';

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
