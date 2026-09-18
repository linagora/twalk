// Reading a failed Gateway call, in the one distinction that keeps being got
// wrong: **did anything answer?**
//
// A screen that says "your Twalk server could not be reached" when the server
// answered — promptly, and with a refusal — sends the user to look at their
// network, their firewall and their deployment, for a problem that was none of
// those. It happened three times in one day: `GET /api/bridges` answered `401`
// in zero milliseconds and screen 3 reported the server unreachable; a bridge
// that refused was reported as a bridge that could not be found (#116); and a
// correct password was reported as refused.
//
// So there are three outcomes, not two, and they mean different things to the
// person reading the screen:
//
//   - **unreachable** — nothing answered. The request did not complete: no
//     status, no body. This is the one that is about the network, and the
//     honest advice is to check the deployment is running.
//   - **session-refused** — it answered `401`, and the client wrapper's
//     refresh could not save the session (`$lib/api/client.ts`) — or this
//     browser never had one. Nothing is wrong with the deployment or the
//     network; it needs to sign in, and a screen must say so rather than
//     describe a failure.
//   - **refused** — it answered, with something else. The deployment has a
//     problem of its own, and retrying is the useful offer.
//
// Not an error type and not a thrown thing: a screen reads this and chooses
// its own words, because the same refusal reads differently on a login screen
// and on a dashboard.

/** What a failed call turned out to be. */
export type ApiTrouble = 'unreachable' | 'session-refused' | 'refused';

/**
 * Reads one `openapi-fetch` result, or `null` for a call that threw.
 *
 * The convention every caller uses: `await gateway.GET(…).catch(() => null)`.
 * `openapi-fetch` lets a transport failure reject — it has no status to report
 * — so `null` *is* "nothing answered", and anything else answered.
 */
export function troubleOf(result: { response: Response } | null): ApiTrouble {
	if (result === null) {
		return 'unreachable';
	}
	// A `401` that reaches a screen has already been through the wrapper's
	// refresh and survived it: the session is gone — or was never there — but
	// the server is not.
	return result.response.status === 401 ? 'session-refused' : 'refused';
}
