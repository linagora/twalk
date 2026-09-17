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
 * single-label host is a LAN name, not a Twalk deployment), each label 1–63
 * characters of letters, digits and hyphens, not starting or ending with a
 * hyphen, and a last label that is not all digits (that would be an IP
 * address, which has no certificate and so no secure context).
 */
export function isValidDomain(value: string): boolean {
	const host = normaliseDomain(value);
	if (host.length === 0 || host.length > 253) {
		return false;
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
 * The bare hostname behind what the user typed: a scheme, a path, a port, a
 * trailing dot, surrounding space and upper case all removed. Idempotent, and
 * it never throws — an unparseable value comes back as itself, for
 * [`isValidDomain`] to reject.
 */
export function normaliseDomain(value: string): string {
	let host = value.trim().toLowerCase();
	host = host.replace(/^[a-z][a-z0-9+.-]*:\/\//, '');
	host = host.split('/')[0] ?? '';
	host = host.split('?')[0] ?? '';
	host = host.split('#')[0] ?? '';
	// A port, but not the colon of an IPv6 literal, which `isValidDomain`
	// rejects anyway.
	host = host.replace(/:\d+$/, '');
	host = host.replace(/\.$/, '');
	return host;
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
