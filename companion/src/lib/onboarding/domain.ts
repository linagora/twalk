// The Twalk domain the user types on screen 1.
//
// The wireframes ask for inline validation — "must be a valid hostname" — and
// for a CTA that stays disabled until it passes. That validation is pure and
// lives here; the store beside it is how screen 1 hands the value to the next
// screen.
//
// Not sensitive, but not nothing either: it is remembered in `sessionStorage`
// so a reload of the next screen does not send the user back to type it again,
// and it dies with the tab. Nothing else about the user's progress is stored —
// spec #65 is explicit that onboarding progress is derived from what exists on
// the Gateway, never from a browser-side state machine.

import { writable } from 'svelte/store';

const STORAGE_KEY = 'twalk:domain';

/**
 * Whether this is a hostname the Companion can talk to.
 *
 * Deliberately a hostname and not a URL: the wireframes' placeholder is
 * `example.com`, and a user who pastes `https://example.com/` has given us a
 * hostname with decoration, which [`normaliseDomain`] removes before this is
 * asked.
 *
 * The rules are the DNS ones that matter here: at least two labels (a
 * single-label host on the public internet is a LAN name, not a Twalk
 * deployment), each label 1–63 characters of letters, digits and hyphens, not
 * starting or ending with a hyphen, and a last label that is not all digits
 * (that would be an IP address, which has no certificate and so no secure
 * context).
 *
 * **Loopback is the exception**, and it is not a convenience: `localhost`,
 * `*.localhost` and `127.0.0.0/8` are secure contexts by browser rule, which
 * is the very reason the Companion's own test suite runs without HTTPS. A
 * deployment reached on loopback is one a developer is running, or the one
 * Playwright drives, and it carries a port — so a port survives normalisation
 * there and nowhere else. See [`homeserverBaseUrl`], which is where the
 * difference is spent.
 */
export function isValidDomain(value: string): boolean {
	const authority = normaliseDomain(value);
	if (authority.length === 0 || authority.length > 253) {
		return false;
	}
	const { host, port } = splitAuthority(authority);
	if (port !== null && !isLoopbackHost(host)) {
		return false;
	}
	if (isLoopbackHost(host)) {
		return true;
	}
	const labels = host.split('.');
	if (labels.length < 2) {
		return false;
	}
	if (/^[0-9]+$/.test(labels[labels.length - 1] ?? '')) {
		return false;
	}
	return labels.every((label) => /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(label));
}

/**
 * The bare hostname behind what the user typed: a scheme, a path, a trailing
 * dot, surrounding space and upper case all removed — and the port too, unless
 * the host is a loopback one, where the port is how the deployment is reached
 * at all. Idempotent, and it never throws — an unparseable value comes back as
 * itself, for [`isValidDomain`] to reject.
 */
export function normaliseDomain(value: string): string {
	let authority = value.trim().toLowerCase();
	authority = authority.replace(/^[a-z][a-z0-9+.-]*:\/\//, '');
	authority = authority.split('/')[0] ?? '';
	authority = authority.split('?')[0] ?? '';
	authority = authority.split('#')[0] ?? '';
	const { host, port } = splitAuthority(authority);
	const bare = host.replace(/\.$/, '');
	return port !== null && isLoopbackHost(bare) ? `${bare}:${port}` : bare;
}

/**
 * Whether this host is one the browser treats as a secure context without a
 * certificate. `*.localhost` resolves to loopback without DNS in every browser
 * of the baseline, which is what makes `twalk.localhost:19148` a usable
 * spelling for a deployment running on this machine.
 */
export function isLoopbackHost(host: string): boolean {
	return (
		host === 'localhost' ||
		host.endsWith('.localhost') ||
		host === '::1' ||
		host === '[::1]' ||
		/^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(host)
	);
}

/**
 * The base URL of the Matrix homeserver this domain stands for, before
 * `.well-known` delegation is consulted (`$lib/matrix/discovery.ts` does
 * that). HTTPS everywhere; plain HTTP on loopback, because a deployment on
 * this machine has no certificate and needs none.
 */
export function homeserverBaseUrl(value: string): string {
	const authority = normaliseDomain(value);
	const { host } = splitAuthority(authority);
	return `${isLoopbackHost(host) ? 'http' : 'https'}://${authority}`;
}

/** Splits `host[:port]`, leaving an IPv6 literal's colons alone. */
function splitAuthority(authority: string): { host: string; port: string | null } {
	const match = /^(.*?):(\d+)$/u.exec(authority);
	if (match === null || match[1] === undefined || match[1].includes(':')) {
		return { host: authority, port: null };
	}
	return { host: match[1], port: match[2] ?? null };
}

export const domain = writable<string>('');

/** Reads the remembered domain. Browser-only; called from a component. */
export function restoreDomain(): void {
	try {
		const stored = window.sessionStorage.getItem(STORAGE_KEY);
		if (stored !== null && isValidDomain(stored)) {
			domain.set(stored);
		}
	} catch {
		// Storage can be switched off; the user retypes it.
	}
}

/** Remembers the domain for the rest of this tab's life. Browser-only. */
export function rememberDomain(value: string): void {
	try {
		window.sessionStorage.setItem(STORAGE_KEY, value);
	} catch {
		// See above.
	}
}
