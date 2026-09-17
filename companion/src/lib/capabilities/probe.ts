// Measures what this browser can do. Browser-only, and the only module in the
// app that touches `indexedDB`, `isSecureContext` or `crypto.subtle` — always
// inside a function, never at module scope, and imported dynamically by its
// callers (`await import('$lib/capabilities/probe')`).
//
// That is not style: prerendering imports every statically reachable module in
// Node, where `indexedDB` does not exist, so a module-scope reference here
// would break `npm run build` rather than fail at runtime.

import type { CapabilityProbe, StorageState } from './report';

/**
 * How long to wait for IndexedDB to answer before calling it blocked.
 *
 * An open request that neither succeeds nor errors is a real state: Safari has
 * shipped it, and a browser that has switched storage off can leave the
 * request pending forever. Without a deadline the gate would spin instead of
 * explaining itself, which is the failure mode ADR 0014 wrote the gate to
 * avoid.
 */
const INDEXED_DB_TIMEOUT_MS = 3000;

export async function probeCapabilities(): Promise<CapabilityProbe> {
	return {
		secureContext: window.isSecureContext === true,
		webAssembly:
			typeof WebAssembly === 'object' &&
			typeof WebAssembly.instantiateStreaming === 'function',
		cryptoSubtle:
			typeof globalThis.crypto === 'object' &&
			typeof globalThis.crypto?.subtle?.digest === 'function' &&
			typeof globalThis.crypto?.getRandomValues === 'function',
		indexedDb: await probeIndexedDb(),
		// `typeof`, not `in`: Lockdown Mode leaves the property in place and
		// empties it, and `'serviceWorker' in navigator` is true either way.
		serviceWorker: typeof (navigator as { serviceWorker?: unknown }).serviceWorker === 'object',
		// `navigator.locks` is not in every lib.dom we might compile against.
		webLocks: typeof (navigator as { locks?: unknown }).locks === 'object'
	};
}

/**
 * Opens a throwaway database, because the presence of `window.indexedDB` says
 * nothing: Lockdown Mode and a blocked-site-data setting both leave the object
 * in place and make `open` fail — or hang.
 */
async function probeIndexedDb(): Promise<StorageState> {
	let factory: IDBFactory | undefined;
	try {
		// Reading the property can itself throw in a hardened browser.
		factory = window.indexedDB ?? undefined;
	} catch {
		return 'blocked';
	}
	if (factory === undefined || typeof factory.open !== 'function') {
		return 'missing';
	}

	const name = 'twalk-companion-probe';
	return new Promise<StorageState>((resolve) => {
		let settled = false;
		const settle = (state: StorageState) => {
			if (!settled) {
				settled = true;
				resolve(state);
			}
		};

		const timer = setTimeout(() => settle('blocked'), INDEXED_DB_TIMEOUT_MS);

		let request: IDBOpenDBRequest;
		try {
			request = factory.open(name, 1);
		} catch {
			clearTimeout(timer);
			settle('blocked');
			return;
		}

		request.onsuccess = () => {
			clearTimeout(timer);
			try {
				request.result.close();
				// Leave nothing behind: the gate is a check, not a store.
				factory.deleteDatabase(name);
			} catch {
				// Deleting is a courtesy; the answer is already known.
			}
			settle('available');
		};
		request.onerror = () => {
			clearTimeout(timer);
			settle('blocked');
		};
		// Firefox in a private window fires neither: the timeout covers it.
	});
}
