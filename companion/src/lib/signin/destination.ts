// Where the sign-in screen sends the user afterwards.
//
// Its own module, and tested, because it is the one place in the Companion
// where a value from the address bar decides a navigation — and the
// navigation happens with a session that was just created. An absolute URL
// accepted here would make `/signin` an open redirect that hands a freshly
// signed-in user to somebody else's page, with `next` supplied by whoever
// wrote the link.
//
// So the rule is a whitelist, not a blacklist: a path on this origin, or the
// default. Everything else is not rejected with an explanation — it is simply
// not used, because a user who followed a bad link wants to be signed in, not
// lectured.

/** The default, for a sign-in that carries no destination. */
export const DEFAULT_DESTINATION = '/dashboard';

/**
 * The path to go to after signing in, read from a URL's `next` parameter.
 *
 * Accepts only a path on this origin. `//evil.example` is rejected too: a
 * protocol-relative URL starts with a slash and is not a path.
 */
export function destinationFrom(url: URL): string {
	const next = url.searchParams.get('next');
	if (next === null || !next.startsWith('/') || next.startsWith('//')) {
		return DEFAULT_DESTINATION;
	}
	// A backslash is a slash to some URL parsers, so `/\evil.example` is the
	// same trick spelled differently.
	if (next.startsWith('/\\')) {
		return DEFAULT_DESTINATION;
	}
	return next;
}
