// A read that does not settle on "unknown".
//
// The network grid and the management screen read `GET /api/bridges` once, on
// mount, and rendered whatever came back for as long as the page was open. A
// bridge the Gateway could not ask at that instant — one `whoami` that timed
// out while the bridge was starting — therefore read as *unknown* until the
// user reloaded, which is a transient failure shown as a permanent one. #148
// found it as a test that failed one run in three; a user finds it as a card
// that says their working link cannot be read, and stays that way.
//
// So a read whose answer is transient is asked again, on its own: a short
// backoff first (the bridge that was starting has usually finished), then a
// slower cadence, and immediately when the tab comes back into view or the
// browser comes back online — the two moments a stale "unknown" is most likely
// to be looked at. It stops the moment the answer is known, and it never
// re-asks a refusal: a `401` is repaired centrally (#111) and a `4xx` is an
// answer, not an outage — the caller's `unsettled()` decides what is transient.
//
// Pure scheduling, no fetch of its own, so the cadence is a unit test and each
// screen keeps its own read.

/** Delays before the 1st, 2nd, 3rd… re-read, in milliseconds. The last repeats. */
export const REREAD_DELAYS_MS: readonly number[] = [2_000, 5_000, 10_000, 30_000];

/** The delay before re-read number `attempt` (1-based). */
export function rereadDelayMs(attempt: number): number {
	const last = REREAD_DELAYS_MS.length - 1;
	const index = Math.min(Math.max(attempt - 1, 0), last);
	return REREAD_DELAYS_MS[index] ?? 30_000;
}

export interface Rereader {
	/** Stops every scheduled re-read and every listener. Idempotent. */
	stop(): void;
}

/** The two event sources, injectable so the schedule is testable without a browser. */
export interface RereadEnvironment {
	/** Fires `visibilitychange`; asked whether the page is hidden. */
	document: Pick<Document, 'addEventListener' | 'removeEventListener'> & {
		readonly visibilityState: DocumentVisibilityState;
	};
	/** Fires `online`. */
	window: Pick<Window, 'addEventListener' | 'removeEventListener'>;
}

/**
 * Runs `read` now, then again while `unsettled()` says the last answer was
 * transient — on the backoff above, and on the tab becoming visible or the
 * browser coming online. `read` is never run twice at once.
 */
export function rereadWhileUnsettled(
	read: () => Promise<void>,
	unsettled: () => boolean,
	environment: RereadEnvironment = { document, window }
): Rereader {
	let stopped = false;
	let attempt = 0;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let inFlight: Promise<void> | null = null;

	const clear = () => {
		if (timer !== null) {
			clearTimeout(timer);
			timer = null;
		}
	};

	const run = async () => {
		if (stopped) {
			return;
		}
		inFlight ??= read().finally(() => {
			inFlight = null;
		});
		await inFlight;
		if (stopped) {
			return;
		}
		clear();
		if (unsettled()) {
			attempt += 1;
			timer = setTimeout(() => {
				timer = null;
				void run();
			}, rereadDelayMs(attempt));
		} else {
			// Known. The next unsettled answer, if any, starts the backoff over.
			attempt = 0;
		}
	};

	const onVisibility = () => {
		if (environment.document.visibilityState !== 'hidden' && unsettled()) {
			void run();
		}
	};
	const onOnline = () => {
		if (unsettled()) {
			void run();
		}
	};
	environment.document.addEventListener('visibilitychange', onVisibility);
	environment.window.addEventListener('online', onOnline);

	void run();

	return {
		stop() {
			stopped = true;
			clear();
			environment.document.removeEventListener('visibilitychange', onVisibility);
			environment.window.removeEventListener('online', onOnline);
		}
	};
}
