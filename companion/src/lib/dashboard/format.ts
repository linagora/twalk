// The two formatting decisions screen 5 makes, kept out of the markup so they
// can be tested against a fixed clock.
//
// `Intl.RelativeTimeFormat` rather than a string table: "2 minutes ago" and
// "il y a 2 minutes" are the platform's job, and a hand-rolled table would be
// a third catalogue to keep in step with the other two.

import { cardFor } from '$lib/networks/catalogue';
import type { MessageKey } from '$lib/i18n';

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * "just now", "12 minutes ago", "3 days ago" — in the locale in force.
 *
 * `null` in, `null` out: a time nobody reported is not "1 January 1970", and
 * every caller on this screen has to render the absence rather than a date.
 */
export function relativeTime(
	iso: string | null,
	now: number,
	locale: string
): string | null {
	if (iso === null) {
		return null;
	}
	const at = Date.parse(iso);
	if (Number.isNaN(at)) {
		return null;
	}
	const format = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
	const delta = at - now;
	const magnitude = Math.abs(delta);
	if (magnitude < MINUTE) {
		return format.format(Math.round(delta / 1000), 'second');
	}
	if (magnitude < HOUR) {
		return format.format(Math.round(delta / MINUTE), 'minute');
	}
	if (magnitude < DAY) {
		return format.format(Math.round(delta / HOUR), 'hour');
	}
	return format.format(Math.round(delta / DAY), 'day');
}

/**
 * The user-facing name of a network, from the catalogue screen 3 already
 * draws. A network the catalogue has no card for renders as its own value
 * rather than as a blank — the Gateway's `network` enum can grow without this
 * screen going silent.
 */
export function networkNameKey(network: string): MessageKey | null {
	return cardFor(network)?.titleKey ?? null;
}
