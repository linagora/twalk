// The `loginToken` an SSO round trip comes back with, taken out of the address
// bar before anything decides what to render.
//
// # Why this is not the Matrix screen's business
//
// It was, and that is the defect (#135). `/networks/matrix` stripped the token
// in `onMount` — "a login token is a credential, and a copied URL must not
// carry one" — and the route never mounted: the tab election showed the
// tab-lock screen instead, so the layout rendered that and the page it guards
// never existed. Measured, with the token still in the address bar afterwards:
//
//     URL after mount: http://127.0.0.1:8083/networks/matrix?loginToken=abc123
//
// **A precaution that only runs when the page it guards is allowed to run is
// not a precaution.** So it runs at the app's own start, before the election,
// before the capability gate, before any route: the root layout calls
// [`captureLoginToken`] while it initialises, which is before any screen
// exists, and the screen that wants the token calls [`takeLoginToken`].
//
// The token is kept in this module rather than in the URL because it is still
// needed: it is single-use, it is exchanged against the homeserver that issued
// it, and holding it in memory is what lets the address bar be clean while the
// exchange is still possible. A reload loses it, which is correct — a login
// token that survived a reload would be a credential at rest.

import { loginTokenFrom } from './login';

/** The token this page load arrived with, until somebody takes it. */
let captured: string | null = null;

/**
 * Reads the login token out of the current URL and removes it from the address
 * bar. Browser-only, idempotent, and safe to call before anything is rendered.
 *
 * Returns what it found, for a caller that wants to know.
 */
export function captureLoginToken(): string | null {
	if (typeof window === 'undefined') {
		return null;
	}
	const here = new URL(window.location.href);
	const token = loginTokenFrom(here);
	if (token === null) {
		return null;
	}
	captured = token;
	here.searchParams.delete('loginToken');
	try {
		// The path, the remaining query and the fragment: same page, one
		// credential fewer, and no navigation.
		window.history.replaceState(null, '', `${here.pathname}${here.search}${here.hash}`);
	} catch {
		// A browser that refuses `replaceState` still must not be handed the
		// token twice; it is captured either way.
	}
	return token;
}

/**
 * Takes the captured token, once. A second caller gets `null`, which is what a
 * single-use credential should look like to everybody but its first reader.
 */
export function takeLoginToken(): string | null {
	const token = captured;
	captured = null;
	return token;
}
