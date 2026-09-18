// Reading the Google cookies screen 3c asks for, out of whatever the user
// managed to copy.
//
// # Why the user is copying cookies at all
//
// Google deprecated the QR login for third-party Google Messages clients in
// 2024, so mautrix-gmessages signs in with a Google session cookie instead
// (docs.mau.fi). The wireframe imagined an in-app browser view doing the
// extraction; a PWA has no such thing, and no page may read another origin's
// cookies — that is the same-origin policy, and working around it would be a
// security hole rather than a feature. So the honest v0.1 shape is: the user
// signs in to Google Messages Web themselves, copies their session cookies,
// and pastes them here.
//
// # What this module will accept
//
// Three spellings, because the user will arrive with whichever their route
// produced:
//
//   - what `document.cookie` returns, and what the Network tab shows as a
//     `Cookie:` header — `SID=…; HSID=…; SSID=…`;
//   - a JSON object — `{"SID": "…", "HSID": "…"}`;
//   - a cookie-extension export — `[{"name": "SID", "value": "…"}, …]`.
//
// Anything else is a parse failure the screen names, rather than a submission
// the bridge refuses a minute later.
//
// # Where they go
//
// Straight into `POST /api/bridges/{id}/login/submit`, which relays them to the
// bridge and forgets them (ADR 0011). **Nothing here writes them anywhere**: no
// storage, no store, no log. The textarea is cleared as soon as the submission
// is made, and the values never leave the variable they were parsed into.

/** A cookie jar: name to value, in the shape the bridge's step takes. */
export type CookieJar = Record<string, string>;

export type CookieParse =
	| { ok: true; cookies: CookieJar }
	| { ok: false; reason: 'empty' | 'unreadable' };

/**
 * Reads a pasted blob into a cookie jar.
 *
 * Values are taken verbatim — a Google session cookie contains `/`, `-`, `_`
 * and `=`, and "cleaning" one is how a login fails with no visible cause.
 */
export function parseCookies(pasted: string): CookieParse {
	const text = pasted.trim();
	if (text === '') {
		return { ok: false, reason: 'empty' };
	}

	if (text.startsWith('{') || text.startsWith('[')) {
		try {
			const parsed: unknown = JSON.parse(text);
			const jar = fromJson(parsed);
			return Object.keys(jar).length > 0 ? { ok: true, cookies: jar } : { ok: false, reason: 'unreadable' };
		} catch {
			return { ok: false, reason: 'unreadable' };
		}
	}

	const jar: CookieJar = {};
	// `;` separates pairs; a newline does too, since a user pasting from a
	// table gets one per line. A `=` inside the value is kept.
	for (const pair of text.split(/[;\n\r]+/u)) {
		const trimmed = pair.trim().replace(/^Cookie:\s*/iu, '');
		if (trimmed === '') {
			continue;
		}
		const at = trimmed.indexOf('=');
		if (at <= 0) {
			continue;
		}
		const name = trimmed.slice(0, at).trim();
		const value = trimmed.slice(at + 1).trim();
		if (name !== '' && value !== '') {
			jar[name] = value;
		}
	}
	return Object.keys(jar).length > 0 ? { ok: true, cookies: jar } : { ok: false, reason: 'unreadable' };
}

function fromJson(parsed: unknown): CookieJar {
	const jar: CookieJar = {};
	if (Array.isArray(parsed)) {
		for (const entry of parsed) {
			if (entry === null || typeof entry !== 'object') {
				continue;
			}
			const record = entry as Record<string, unknown>;
			const name = record['name'];
			const value = record['value'];
			if (typeof name === 'string' && typeof value === 'string' && name !== '') {
				jar[name] = value;
			}
		}
		return jar;
	}
	if (parsed !== null && typeof parsed === 'object') {
		for (const [name, value] of Object.entries(parsed as Record<string, unknown>)) {
			if (typeof value === 'string' && name !== '') {
				jar[name] = value;
			}
		}
	}
	return jar;
}

/**
 * Which of the cookies the bridge asked for are not in the jar.
 *
 * The screen names them rather than saying "invalid": a user who copied six of
 * seven needs to know which one, and the seventh is usually the one their
 * browser hid.
 */
export function missing(required: readonly string[], jar: CookieJar): string[] {
	return required.filter((name) => (jar[name] ?? '') === '');
}
