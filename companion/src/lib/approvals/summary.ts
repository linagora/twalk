// What the **dashboard** may know about suggestions, which is a number.
//
// #100's first acceptance criterion is a rule about two screens: *"the home
// screen is what gets unlocked on a train, and an approval queue shows the
// proposed text. The dashboard carries a count and a link; the content lives
// here, opened deliberately."* That is the same reasoning that emptied the
// activity feed of sender identities (#74) and that drops `pending.contacts`
// in `$lib/dashboard/load.ts` — and it is enforced the same way, by dropping
// the text **at the seam** rather than by a component happening not to render
// it.
//
// This module is that seam. `GET /api/suggestions` answers with every
// proposed reply in full; [`summarise`] turns it into two numbers and a
// boolean, and nothing downstream of it can render a word of what a persona
// wrote. The ticket asks for a test that greps the rendered dashboard for a
// suggestion's text and finds nothing; the reason that test passes is this
// function, and `summary.test.ts` asserts the property directly on the value
// rather than on the pixels.

import type { Listing } from './rows';
import { waitingCount } from './rows';

export interface Waiting {
	/** How many rows are waiting for the user. */
	count: number;
	/**
	 * Whether `limit` cut the read, so the count is a floor rather than a
	 * total. A chip that said "12" when it meant "at least 12" would be the
	 * bound that lies, one screen further out.
	 */
	atLeast: boolean;
}

/**
 * Every suggestion, reduced to what a home screen may hold.
 *
 * The return type is the enforcement: there is no member here that a text
 * could be carried in, so adding one would be a visible change to a file whose
 * whole comment is about why it has none.
 */
export function summarise(listing: Listing, dismissed: ReadonlySet<string>): Waiting {
	return { count: waitingCount(listing, dismissed), atLeast: listing.truncated };
}
