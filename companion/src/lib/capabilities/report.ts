// What the browser can do, and — when something is missing — *why*, which is
// the part that matters.
//
// ADR 0014 and spec #65 ask for a gate before onboarding starts rather than a
// failure in the middle of it, and they name the case that makes a plain
// feature list useless: iOS Lockdown Mode switches off IndexedDB, service
// workers and Web Locks while leaving WebAssembly running. "IndexedDB is
// missing" is true there and helps nobody; "this looks like Lockdown Mode,
// here is the setting" is actionable. An insecure origin has the same
// property — it takes out storage, service workers and Web Crypto at once, and
// the single useful sentence is about HTTPS.
//
// So this module turns a probe into a *cause*, and the gate screen shows the
// cause first and the list second.
//
// Pure, and deliberately so: no browser API is touched here, the probe lives
// in `./probe.ts`, and this file is unit-tested against invented probes —
// including ones no browser we own would produce.

/** Whether a storage API is there, gone, or there but refusing to work. */
export type StorageState = 'available' | 'missing' | 'blocked';

/** What `./probe.ts` measures. One member per thing a browser can withhold. */
export interface CapabilityProbe {
	/** `window.isSecureContext`: HTTPS, or a localhost origin. */
	secureContext: boolean;
	/** `WebAssembly`, with `instantiateStreaming` — the Matrix crypto stack. */
	webAssembly: boolean;
	/** `crypto.subtle`: the randomness the recovery key is made of. */
	cryptoSubtle: boolean;
	/**
	 * IndexedDB. `'blocked'` means the API is present but an open failed or
	 * never answered — a private window, blocked site data, Lockdown Mode.
	 * Distinguishing it from `'missing'` is what lets the cause be named.
	 */
	indexedDb: StorageState;
	/** `navigator.serviceWorker`: installability, nothing else. */
	serviceWorker: boolean;
	/** `navigator.locks`: the single-tab lock ADR 0014 relies on. */
	webLocks: boolean;
}

/** A row the gate screen can draw, and the i18n key suffix for its label. */
export type CapabilityId =
	| 'secure-context'
	| 'webassembly'
	| 'crypto-subtle'
	| 'storage'
	| 'service-worker'
	| 'tab-lock';

/**
 * Why the browser cannot do the job. One sentence of advice per value, in the
 * catalogues under `gate.cause.<value>`.
 */
export type Cause =
	| 'none'
	/** Not HTTPS and not localhost: everything else follows from this. */
	| 'insecure-context'
	/** WebAssembly runs, storage and workers do not: iOS Lockdown Mode. */
	| 'lockdown-mode'
	/** IndexedDB is there but refuses: a private window, or blocked site data. */
	| 'storage-blocked'
	/** Something is simply absent: a browser outside the supported baseline. */
	| 'unsupported-browser';

export interface CapabilityReport {
	/** Whether onboarding may start. False when `missing` is not empty. */
	ok: boolean;
	/** What onboarding cannot do without. Blocking. */
	missing: CapabilityId[];
	/** What only costs the user a convenience. Not blocking. */
	degraded: CapabilityId[];
	cause: Cause;
	/** The state of every capability, in display order, blockers first. */
	rows: CapabilityRow[];
}

export interface CapabilityRow {
	id: CapabilityId;
	state: StorageState;
	/** Whether onboarding is impossible without it. */
	required: boolean;
}

/**
 * Reads a probe. The blocking set is the one spec #65 fixes — secure context,
 * WebAssembly, IndexedDB — plus Web Crypto, which is the same promise stated
 * differently (a recovery key needs a CSPRNG) and which no browser in the
 * baseline withholds on a secure origin.
 *
 * Service workers and Web Locks are *not* blocking. Missing them costs
 * installation and the two-tab warning, and an iOS user in Lockdown Mode who
 * has turned it off for this site should not be stopped because a service
 * worker is still disabled elsewhere.
 */
export function reportCapabilities(probe: CapabilityProbe): CapabilityReport {
	const rows: CapabilityRow[] = [
		{
			id: 'secure-context',
			state: probe.secureContext ? 'available' : 'missing',
			required: true
		},
		{
			id: 'webassembly',
			state: probe.webAssembly ? 'available' : 'missing',
			required: true
		},
		{ id: 'storage', state: probe.indexedDb, required: true },
		{
			id: 'crypto-subtle',
			state: probe.cryptoSubtle ? 'available' : 'missing',
			required: true
		},
		{
			id: 'service-worker',
			state: probe.serviceWorker ? 'available' : 'missing',
			required: false
		},
		{
			id: 'tab-lock',
			state: probe.webLocks ? 'available' : 'missing',
			required: false
		}
	];

	const missing = rows
		.filter((row) => row.required && row.state !== 'available')
		.map((row) => row.id);
	const degraded = rows
		.filter((row) => !row.required && row.state !== 'available')
		.map((row) => row.id);

	return {
		ok: missing.length === 0,
		missing,
		degraded,
		cause: causeOf(probe, missing.length === 0),
		rows
	};
}

/**
 * The single most useful thing to tell the user. Order matters: an insecure
 * origin explains away the storage and worker failures it caused, so it is
 * checked first, and Lockdown Mode's signature is only meaningful on a secure
 * origin.
 */
function causeOf(probe: CapabilityProbe, ok: boolean): Cause {
	if (!probe.secureContext) {
		return 'insecure-context';
	}

	// Lockdown Mode's exact signature (ADR 0014): WebAssembly still runs while
	// IndexedDB, service workers and Web Locks are all gone at once. No
	// supported browser loses all three for any other reason on a secure
	// origin, and it is worth naming because the fix is one iOS setting.
	if (
		probe.webAssembly &&
		probe.indexedDb !== 'available' &&
		!probe.serviceWorker &&
		!probe.webLocks
	) {
		return 'lockdown-mode';
	}

	// IndexedDB answers, but refuses to open: site data is blocked for this
	// origin, which is what a private window does.
	if (probe.indexedDb === 'blocked') {
		return 'storage-blocked';
	}

	return ok ? 'none' : 'unsupported-browser';
}
