// The diagnostics text is the only thing the Companion offers a user to send
// when something breaks, and the only thing that could leak if it said too
// much. Both halves are asserted here: that it carries what a maintainer
// needs, and that it carries nothing the user would regret pasting.

import { describe, expect, it } from 'vitest';

import { buildDiagnostics } from './diagnostics';
import { reportCapabilities } from './capabilities/report';

const capabilities = reportCapabilities({
	secureContext: true,
	webAssembly: true,
	cryptoSubtle: true,
	indexedDb: 'blocked',
	serviceWorker: false,
	webLocks: false
});

const input = {
	appBuild: '1789600000000',
	expectedGatewayVersion: '0.1.0',
	health: { version: '0.2.0', revision: 'abc1234', companionBuild: '1789839442194' },
	handshake: { kind: 'mismatch', expected: '0.1.0', actual: '0.2.0' } as const,
	capabilities,
	locale: 'fr',
	userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)',
	installed: true,
	generatedAt: new Date('2026-09-18T07:30:00.000Z')
};

describe('buildDiagnostics', () => {
	it('names both versions and what the handshake made of them', () => {
		const text = buildDiagnostics(input);
		expect(text).toContain('built for gateway: 0.1.0');
		expect(text).toContain('gateway version: 0.2.0');
		expect(text).toContain('gateway revision: abc1234');
		expect(text).toContain('mismatch (built for 0.1.0, gateway 0.2.0)');
	});

	it('lists every capability with its state and whether it was required', () => {
		const text = buildDiagnostics(input);
		expect(text).toContain('capabilities cause: lockdown-mode');
		expect(text).toContain('storage: blocked (required)');
		expect(text).toContain('service-worker: missing');
		expect(text).not.toContain('service-worker: missing (required)');
	});

	it('says so when the Gateway did not answer, rather than inventing a version', () => {
		const text = buildDiagnostics({
			...input,
			health: null,
			handshake: { kind: 'unreachable', reason: 'health answered 502' }
		});
		expect(text).toContain('gateway version: unknown');
		expect(text).toContain('unreachable (health answered 502)');
	});

	it('is reproducible, because the timestamp comes from the caller', () => {
		expect(buildDiagnostics(input)).toBe(buildDiagnostics(input));
		expect(buildDiagnostics(input)).toContain('generated: 2026-09-18T07:30:00.000Z');
	});

	it('states that nothing was transmitted', () => {
		// The user is about to paste this somewhere. They should be able to
		// see, in the text itself, that it was not also sent anywhere.
		expect(buildDiagnostics(input)).toContain('No telemetry is collected');
	});

	it('carries nothing about the owner or their conversations', () => {
		// The input has no place to put a Matrix ID, a domain or a token, and
		// that is the design. This test fails the day someone adds one.
		const text = buildDiagnostics(input);
		expect(text).not.toMatch(/@[a-z0-9._=\-/]+:/i);
		expect(text.toLowerCase()).not.toContain('token');
		expect(Object.keys(input)).toEqual([
			'appBuild',
			'expectedGatewayVersion',
			'health',
			'handshake',
			'capabilities',
			'locale',
			'userAgent',
			'installed',
			'generatedAt'
		]);
	});
});
