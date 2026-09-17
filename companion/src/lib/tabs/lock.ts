// One tab at a time, elected with a Web Lock.
//
// ADR 0014, and matrix-js-sdk says it in capitals: "the cryptography stack is
// not thread-safe. Having multiple `MatrixClient` instances connected to the
// same Indexed DB will cause data corruption and decryption failures." Two
// tabs of the Companion is exactly that, and the damage is silent — a corrupt
// store does not announce itself, it fails to decrypt something weeks later.
//
// So: the first tab takes an exclusive Web Lock and holds it for as long as it
// is open. Any other tab fails to take it and shows the "open in another tab"
// screen instead of the app. A lock is released automatically when its tab
// closes or crashes, which is the property no hand-rolled `localStorage`
// mutex has.
//
// **Taking over** is the other half, because a user with a forgotten tab on
// another virtual desktop must not be stuck. The waiting tab asks over a
// `BroadcastChannel`; the holder hears it, lets go of its client and its lock,
// and becomes a waiting tab itself. Then the asker's lock request — which is
// queued, not polled — is granted.
//
// Browsers without Web Locks (iOS Lockdown Mode) report `unsupported`, which
// the capability gate has already listed as degraded: they get the app, and
// the risk, rather than a wall.

/** Which tab this is. */
export type TabRole =
	/** Holding the lock: this tab runs the app. */
	| 'active'
	/** Another tab holds it: show the "open in another tab" screen. */
	| 'elsewhere'
	/** No Web Locks in this browser: nothing can be elected. */
	| 'unsupported'
	/** Before the first answer. */
	| 'electing';

const LOCK_NAME = 'twalk-companion-crypto';
const CHANNEL_NAME = 'twalk-companion-tabs';

type LockManager = {
	request: (
		name: string,
		options: { mode?: 'exclusive' | 'shared'; ifAvailable?: boolean; signal?: AbortSignal },
		callback: (lock: unknown | null) => Promise<void>
	) => Promise<void>;
};

function lockManager(): LockManager | null {
	const locks = (navigator as { locks?: LockManager }).locks;
	return typeof locks === 'object' && locks !== null ? locks : null;
}

export interface TabElection {
	/** Called every time the role changes, including the first answer. */
	onRole: (role: TabRole) => void;
	/**
	 * Called when this tab is asked to give the lock up, before it does.
	 * The crypto client must be released here: the lock is a promise about the
	 * store, and letting go of one without the other keeps the store open.
	 */
	onYield?: () => void | Promise<void>;
}

/**
 * Elects this tab, and keeps it elected. Idempotent per page: calling it twice
 * returns the same election.
 */
export function electTab(options: TabElection): { takeOver: () => void; stop: () => void } {
	const locks = lockManager();
	const channel = openChannel();
	let stopped = false;
	/** Resolves the callback holding the lock, which releases it. */
	let release: (() => void) | null = null;

	if (locks === null) {
		options.onRole('unsupported');
		return { takeOver: () => {}, stop: () => channel?.close() };
	}

	const claim = () => {
		if (stopped) {
			return;
		}
		void locks
			.request(LOCK_NAME, { mode: 'exclusive' }, async (lock) => {
				if (lock === null || stopped) {
					return;
				}
				options.onRole('active');
				await new Promise<void>((resolve) => {
					release = resolve;
				});
				release = null;
			})
			.catch(() => {
				// A rejected request means no lock, which is the same
				// situation as another tab holding it.
				if (!stopped) {
					options.onRole('elsewhere');
				}
			});
	};

	// Two requests, and the order is what makes the first answer immediate:
	// `ifAvailable` answers now — `null` when somebody else holds it — and the
	// queued request above waits for its turn without polling.
	void locks
		.request(LOCK_NAME, { mode: 'exclusive', ifAvailable: true }, async (lock) => {
			if (lock === null) {
				options.onRole('elsewhere');
				return;
			}
			// Held only long enough to answer; the queued claim takes over.
		})
		.catch(() => options.onRole('elsewhere'))
		.finally(claim);

	channel?.addEventListener('message', (event: MessageEvent) => {
		if ((event.data as { type?: string })?.type !== 'take-over' || release === null) {
			return;
		}
		void (async () => {
			await options.onYield?.();
			options.onRole('elsewhere');
			release?.();
			// Queue up again, so this tab can be taken back later.
			setTimeout(claim, 0);
		})();
	});

	return {
		takeOver: () => channel?.postMessage({ type: 'take-over' }),
		stop: () => {
			stopped = true;
			release?.();
			channel?.close();
		}
	};
}

function openChannel(): BroadcastChannel | null {
	try {
		return typeof BroadcastChannel === 'function' ? new BroadcastChannel(CHANNEL_NAME) : null;
	} catch {
		return null;
	}
}
