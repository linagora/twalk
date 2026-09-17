// Whether this browser still holds the device's crypto store — and nothing
// else. Browser-only: every caller reaches it through `await import(...)`.
//
// matrix-js-sdk keeps the Rust crypto machine's state in one IndexedDB
// database, named from the prefix it is initialised with:
// `matrix-js-sdk::matrix-sdk-crypto`. ADR 0014 leaves it unencrypted (any key
// protecting it would live in the same browser storage) and treats its loss as
// **normal**: Safari deletes a tab's IndexedDB after seven days without
// interaction unless the Companion is installed to the home screen. So the
// question "is the store still there?" is not diagnostics, it is the branch
// between "carry on" and the store-loss journey.
//
// Asking it without creating the store is the whole difficulty.
// `indexedDB.databases()` is the clean answer and is in every browser of the
// baseline (Safari 14+, current Chrome, Edge and Firefox 126+); where it is
// not, opening the database and watching for an upgrade event tells us the
// same thing, and the upgrade is aborted so the probe leaves nothing behind.

/** The database matrix-js-sdk creates for `cryptoDatabasePrefix` 'matrix-js-sdk'. */
export const CRYPTO_STORE_NAME = 'matrix-js-sdk::matrix-sdk-crypto';

/**
 * True when a crypto store for this origin exists. False after an eviction, a
 * "clear site data", or on a browser that has never run the Companion.
 */
export async function hasCryptoStore(name = CRYPTO_STORE_NAME): Promise<boolean> {
	const factory = indexedDbFactory();
	if (factory === null) {
		return false;
	}

	const enumerate = (factory as { databases?: () => Promise<{ name?: string }[]> }).databases;
	if (typeof enumerate === 'function') {
		try {
			const databases = await enumerate.call(factory);
			return databases.some((database) => database.name === name);
		} catch {
			// Fall through: a browser that lists nothing can still be asked.
		}
	}

	return await existsByOpening(factory, name);
}

/**
 * Deletes the crypto store. Used by the store-loss journey when the user
 * chooses to start the device again from their recovery key — a half-written
 * store is worse than none — and by nothing else. There is no reset flow for
 * the *account* in v0.1.
 */
export async function forgetCryptoStore(name = CRYPTO_STORE_NAME): Promise<void> {
	const factory = indexedDbFactory();
	if (factory === null) {
		return;
	}
	await new Promise<void>((resolve) => {
		let request: IDBOpenDBRequest;
		try {
			request = factory.deleteDatabase(name);
		} catch {
			resolve();
			return;
		}
		request.onsuccess = () => resolve();
		request.onerror = () => resolve();
		request.onblocked = () => resolve();
	});
}

function indexedDbFactory(): IDBFactory | null {
	try {
		return window.indexedDB ?? null;
	} catch {
		// A hardened browser can throw on the property itself; the capability
		// gate has already told the user about it.
		return null;
	}
}

/**
 * The fallback: open the database with no version. A store that does not exist
 * is created at version 1, which fires `upgradeneeded` — so that event *is*
 * the answer "it was not there", and aborting the transaction undoes the
 * creation.
 */
async function existsByOpening(factory: IDBFactory, name: string): Promise<boolean> {
	return await new Promise<boolean>((resolve) => {
		let created = false;
		let request: IDBOpenDBRequest;
		try {
			request = factory.open(name);
		} catch {
			resolve(false);
			return;
		}
		request.onupgradeneeded = (event) => {
			created = true;
			try {
				(event.target as IDBOpenDBRequest).transaction?.abort();
			} catch {
				// Aborting is the courtesy; the answer is already known.
			}
		};
		request.onsuccess = () => {
			try {
				request.result.close();
			} catch {
				// Nothing to do: we only wanted to know.
			}
			resolve(!created);
		};
		request.onerror = () => {
			if (created) {
				// The abort surfaces as an error on the open request.
				void factory.deleteDatabase(name);
			}
			resolve(!created);
		};
	});
}
