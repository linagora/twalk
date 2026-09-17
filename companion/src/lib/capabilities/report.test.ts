// The capability report's whole job is naming a cause, so these cases are the
// causes: a healthy browser, an insecure origin, iOS Lockdown Mode, a private
// window, and a browser that is simply too old. Playwright checks that the
// gate screen renders what this decides; what it decides is checked here,
// against probes no browser we own would produce.

import { describe, expect, it } from 'vitest';

import { reportCapabilities, type CapabilityProbe } from './report';

const healthy: CapabilityProbe = {
	secureContext: true,
	webAssembly: true,
	cryptoSubtle: true,
	indexedDb: 'available',
	serviceWorker: true,
	webLocks: true
};

describe('reportCapabilities', () => {
	it('lets a current browser on a secure origin through', () => {
		const report = reportCapabilities(healthy);
		expect(report).toMatchObject({ ok: true, missing: [], degraded: [], cause: 'none' });
	});

	it('blames the insecure origin, not the storage it took out with it', () => {
		// What a browser on plain http actually looks like: no secure context,
		// and everything that depends on one is gone too. Listing four
		// failures would bury the single fix.
		const report = reportCapabilities({
			...healthy,
			secureContext: false,
			cryptoSubtle: false,
			indexedDb: 'missing',
			serviceWorker: false
		});
		expect(report.ok).toBe(false);
		expect(report.cause).toBe('insecure-context');
		expect(report.missing).toContain('secure-context');
	});

	it('recognises iOS Lockdown Mode by what it leaves running', () => {
		// ADR 0014: Lockdown Mode disables IndexedDB, service workers and Web
		// Locks, and leaves WebAssembly alone. That combination is the
		// signature, and it is worth naming because the fix is one setting.
		const report = reportCapabilities({
			...healthy,
			indexedDb: 'blocked',
			serviceWorker: false,
			webLocks: false
		});
		expect(report.ok).toBe(false);
		expect(report.cause).toBe('lockdown-mode');
		expect(report.missing).toEqual(['storage']);
	});

	it('does not call it Lockdown Mode when WebAssembly is gone too', () => {
		// Same missing storage and workers, but no WebAssembly: that is an old
		// browser, and telling its user about an iOS setting would be a lie.
		const report = reportCapabilities({
			...healthy,
			webAssembly: false,
			indexedDb: 'missing',
			serviceWorker: false,
			webLocks: false
		});
		expect(report.cause).toBe('unsupported-browser');
	});

	it('distinguishes blocked storage from missing storage', () => {
		// A private window: IndexedDB is there and refuses to open, while
		// service workers still exist. Not Lockdown Mode, and not an old
		// browser — the user needs a normal window.
		const report = reportCapabilities({ ...healthy, indexedDb: 'blocked' });
		expect(report.cause).toBe('storage-blocked');
		expect(report.missing).toEqual(['storage']);

		const absent = reportCapabilities({ ...healthy, indexedDb: 'missing' });
		expect(absent.cause).toBe('unsupported-browser');
	});

	it('does not block onboarding for installability or the tab lock', () => {
		// The gate must not stop a user whose browser can do the actual work.
		const report = reportCapabilities({ ...healthy, serviceWorker: false, webLocks: false });
		expect(report.ok).toBe(true);
		expect(report.missing).toEqual([]);
		expect(report.degraded).toEqual(['service-worker', 'tab-lock']);
		expect(report.cause).toBe('none');
	});

	it('blocks on each of the four requirements spec #65 names', () => {
		expect(reportCapabilities({ ...healthy, webAssembly: false }).missing).toContain('webassembly');
		expect(reportCapabilities({ ...healthy, cryptoSubtle: false }).missing).toContain(
			'crypto-subtle'
		);
		expect(reportCapabilities({ ...healthy, indexedDb: 'missing' }).missing).toContain('storage');
		expect(reportCapabilities({ ...healthy, secureContext: false }).missing).toContain(
			'secure-context'
		);
	});

	it('reports every capability as a row, so the screen can name each one', () => {
		const report = reportCapabilities({ ...healthy, indexedDb: 'blocked' });
		expect(report.rows.map((row) => row.id)).toEqual([
			'secure-context',
			'webassembly',
			'storage',
			'crypto-subtle',
			'service-worker',
			'tab-lock'
		]);
		expect(report.rows.find((row) => row.id === 'storage')).toEqual({
			id: 'storage',
			state: 'blocked',
			required: true
		});
	});
});
