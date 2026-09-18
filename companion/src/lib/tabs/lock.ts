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
// # The holder that is there and cannot answer
//
// A take-over used to have no terminal state: the asker posted its message and
// the button spun for ever if nothing came back (#135). The obvious diagnosis
// was "the holder is gone", and it is the wrong one — measured against the live
// deployment, a lock does die with its tab, a reload keeps the app, and an
// external round trip and back keeps the app. What actually happens is the
// opposite: the holder is **alive and cannot answer**. Chrome freezes a
// background tab it thinks you have forgotten (Memory Saver); a frozen tab
// keeps its document, and so keeps its Web Lock, and runs no JavaScript, so it
// never hears the request.
//
// No timeout diagnoses that on its own — which is why `takeOver` does not just
// fail, it reports *which* of the two happened, and the screen names the
// remedy that works on a frozen tab: find it and close it, or restart the
// browser. A spinner names none.
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

/**
 * How long a take-over waits for the holder before saying it did not answer.
 *
 * A live holder yields in well under a second — measured at four seconds for
 * the whole journey including two page loads — so this is generous for the
 * case that works and short enough that the case that never will is not
 * mistaken for slowness. The screen states it, because a deadline the user is
 * not told about is indistinguishable from a hang.
 */
export const TAKE_OVER_TIMEOUT_MS = 5000;

/** What a take-over came to. */
export type TakeOverOutcome =
	/** The holder let go and this tab now runs the app. */
	| 'active'
	/**
	 * Nobody answered within [`TAKE_OVER_TIMEOUT_MS`]. The lock is still held
	 * — a released one would have been granted — so the holder exists and is
	 * not running JavaScript.
	 */
	| 'unanswered';

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
export function electTab(options: TabElection): {
	takeOver: () => Promise<TakeOverOutcome>;
	stop: () => void;
} {
	const locks = lockManager();
	const channel = openChannel();
	let stopped = false;
	/** Resolves the callback holding the lock, which releases it. */
	let release: (() => void) | null = null;
	/** A take-over in flight, waiting to be told this tab became active. */
	let asking: ((outcome: TakeOverOutcome) => void) | null = null;

	/**
	 * Announces a role, and settles a take-over that was waiting for it.
	 * Becoming active *is* the answer: the holder yielded and the queued lock
	 * request was granted.
	 */
	const announce = (role: TabRole) => {
		if (role === 'active' && asking !== null) {
			const settle = asking;
			asking = null;
			settle('active');
		}
		options.onRole(role);
	};

	if (locks === null) {
		announce('unsupported');
		// Nothing was elected, so this tab is already running the app and
		// there is nothing to take over.
		return { takeOver: async () => 'active', stop: () => channel?.close() };
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
				announce('active');
				await new Promise<void>((resolve) => {
					release = resolve;
				});
				release = null;
			})
			.catch(() => {
				// A rejected request means no lock, which is the same
				// situation as another tab holding it.
				if (!stopped) {
					announce('elsewhere');
				}
			});
	};

	// Two requests, and the order is what makes the first answer immediate:
	// `ifAvailable` answers now — `null` when somebody else holds it — and the
	// queued request above waits for its turn without polling.
	void locks
		.request(LOCK_NAME, { mode: 'exclusive', ifAvailable: true }, async (lock) => {
			if (lock === null) {
				announce('elsewhere');
				return;
			}
			// Held only long enough to answer; the queued claim takes over.
		})
		.catch(() => announce('elsewhere'))
		.finally(claim);

	channel?.addEventListener('message', (event: MessageEvent) => {
		if ((event.data as { type?: string })?.type !== 'take-over' || release === null) {
			return;
		}
		void (async () => {
			await options.onYield?.();
			announce('elsewhere');
			release?.();
			// Queue up again, so this tab can be taken back later.
			setTimeout(claim, 0);
		})();
	});

	return {
		/**
		 * Asks the holder to let go, and always comes back with an answer.
		 *
		 * `active` means it did; `unanswered` means the deadline passed with
		 * the lock still held, which — since a browser releases a lock when
		 * its tab dies — means the holder is there and is running nothing.
		 */
		takeOver: () => {
			if (channel === null) {
				// No `BroadcastChannel`: there is no way to ask at all, and a
				// button that pretended to ask would be the spinner again.
				return Promise.resolve<TakeOverOutcome>('unanswered');
			}
			return new Promise<TakeOverOutcome>((resolve) => {
				asking = resolve;
				channel.postMessage({ type: 'take-over' });
				setTimeout(() => {
					if (asking === resolve) {
						asking = null;
						resolve('unanswered');
					}
				}, TAKE_OVER_TIMEOUT_MS);
			});
		},
		stop: () => {
			stopped = true;
			// A take-over still waiting when the page goes away is answered,
			// not dropped: a promise nobody settles is the defect in miniature.
			asking?.('unanswered');
			asking = null;
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
