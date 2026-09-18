// Refusing a suggestion, and the exact size of what that can mean today.
//
// # There is no API for this, and the copy has to say so
//
// #100 asks for three actions per row: approve, edit then approve, and
// **refuse**. Two of those are `POST /api/approvals`. The third is not
// anything: this origin serves no route that refuses a suggestion, and it
// could not usefully serve one without a decision nobody has taken.
//
// The reason is structural rather than an oversight. A suggestion *lives in
// the stream* — the Gateway keeps no copy, deliberately, so that "a replay and
// the list cannot disagree with nobody able to say which is true"
// (`CONTEXT.md`). A refusal recorded at the Gateway would be exactly that
// second store: a fact about a suggestion, held beside the suggestion, that a
// replay could contradict. And a refusal published on the bus would be a new
// contract event type, before the v1.0 freeze, whose only consumer would be
// this screen.
//
// So a refusal here is what this browser can honestly do: **stop showing the
// row**. The suggestion is untouched, the persona is not told, and the
// suggestion expires on its own schedule (ticket #22's window). The screen
// says all three of those in as many words — `approvals.dismiss.meaning` — and
// offers the dismissal back, because a local hiding that could not be undone
// would be worse than one that can.
//
// Whether refusing should teach the persona anything is a product decision
// with a contract change in it, and it belongs to whoever owns #160's
// neighbourhood rather than to the screen that first needed it.
//
// # Why the storage is wrapped this carefully
//
// `localStorage` throws in a private window, under blocked site data and in a
// prerender, and the capability gate already treats storage as a thing a
// browser may not have. A dismissal is a convenience; a screen that failed to
// render because one could not be saved would be trading the whole feature for
// a nicety. So every access is caught, and the pure half — the set arithmetic
// — is separate and tested.

/** Where a dismissal is kept. Per origin, per browser, and nowhere else. */
const KEY = 'twalk:approvals:dismissed';

/**
 * How many dismissals are kept.
 *
 * A dismissal is only meaningful while the suggestion is still in the
 * Gateway's read window; after that the row is gone anyway. So this is a lid
 * on the storage rather than a policy, oldest dropped first.
 */
const LIMIT = 200;

/** Reads a stored value into a set of ids. Pure, so the parsing is testable. */
export function parse(raw: string | null): Set<string> {
	if (raw === null) {
		return new Set();
	}
	try {
		const value: unknown = JSON.parse(raw);
		if (!Array.isArray(value)) {
			return new Set();
		}
		return new Set(value.filter((entry): entry is string => typeof entry === 'string'));
	} catch {
		// Someone else's value, or a truncated write. An empty set shows every
		// suggestion, which is the failure that loses nothing.
		return new Set();
	}
}

/** The value to store for a set of ids, newest last and capped. Pure. */
export function serialise(ids: Iterable<string>): string {
	const all = [...ids];
	return JSON.stringify(all.slice(Math.max(0, all.length - LIMIT)));
}

function storage(): Storage | null {
	try {
		return globalThis.localStorage ?? null;
	} catch {
		return null;
	}
}

/** Every suggestion this browser has dismissed. Empty when storage is unavailable. */
export function dismissed(): Set<string> {
	const store = storage();
	if (store === null) {
		return new Set();
	}
	try {
		return parse(store.getItem(KEY));
	} catch {
		return new Set();
	}
}

/** Hides one row in this browser. Returns the new set, so a caller can render it. */
export function dismiss(id: string): Set<string> {
	const next = dismissed();
	next.add(id);
	write(next);
	return next;
}

/** Puts every dismissed row back. There is nothing at the Gateway to undo. */
export function restoreAll(): Set<string> {
	write(new Set());
	return new Set();
}

function write(ids: Set<string>): void {
	const store = storage();
	if (store === null) {
		return;
	}
	try {
		store.setItem(KEY, serialise(ids));
	} catch {
		// Storage full or refused. The row stays visible, which is the state
		// the user can see and act on.
	}
}
