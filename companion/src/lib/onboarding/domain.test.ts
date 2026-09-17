// Screen 1's "Continue" is disabled until this says yes, so what it accepts is
// the whole gate on the first thing a user types.

import { describe, expect, it } from 'vitest';

import { isValidDomain, normaliseDomain } from './domain';

describe('normaliseDomain', () => {
	it('strips the decoration a user pastes', () => {
		expect(normaliseDomain('https://Example.COM/')).toBe('example.com');
		expect(normaliseDomain('  http://twalk.example.com:8443/setup?x=1 ')).toBe(
			'twalk.example.com'
		);
		expect(normaliseDomain('example.com.')).toBe('example.com');
	});

	it('is idempotent', () => {
		expect(normaliseDomain(normaliseDomain('HTTPS://a.B.fr/x'))).toBe('a.b.fr');
	});

	it('hands anything unparseable back for validation to reject', () => {
		expect(normaliseDomain('not a domain')).toBe('not a domain');
		expect(normaliseDomain('')).toBe('');
	});
});

describe('isValidDomain', () => {
	it('accepts the hostnames a deployment actually has', () => {
		expect(isValidDomain('example.com')).toBe(true);
		expect(isValidDomain('twalk.example.co.uk')).toBe(true);
		expect(isValidDomain('my-hub.example.fr')).toBe(true);
		// Through normalisation, so a pasted URL passes too.
		expect(isValidDomain('https://example.com/')).toBe(true);
	});

	it('refuses a single label', () => {
		// `localhost` or a LAN name is not a Twalk deployment, and it would
		// send the user into a homeserver probe that cannot succeed.
		expect(isValidDomain('localhost')).toBe(false);
		expect(isValidDomain('twalk')).toBe(false);
	});

	it('refuses an IP address', () => {
		// No certificate, so no secure context, so no crypto (ADR 0014).
		expect(isValidDomain('192.168.1.10')).toBe(false);
	});

	it('refuses malformed labels', () => {
		expect(isValidDomain('')).toBe(false);
		expect(isValidDomain('exa mple.com')).toBe(false);
		expect(isValidDomain('-example.com')).toBe(false);
		expect(isValidDomain('example-.com')).toBe(false);
		expect(isValidDomain('example..com')).toBe(false);
		expect(isValidDomain('exa_mple.com')).toBe(false);
	});

	it('refuses a hostname longer than DNS allows', () => {
		expect(isValidDomain(`${'a'.repeat(64)}.com`)).toBe(false);
		expect(isValidDomain(`${`${'a'.repeat(50)}.`.repeat(6)}com`)).toBe(false);
	});
});
