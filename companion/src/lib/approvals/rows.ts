// What the approval screen draws, decided here rather than in the markup.
//
// The screen has one job with a decision in it — *which actions is this row
// allowed to offer* — and that decision is the ticket's hardest requirement,
// so it lives in a module a test can reach.
//
// # Three situations, three answers (#100)
//
// The Gateway answers `standing` as one of three values and it is the whole
// vocabulary: `approvable`, `expired`, `approved`. A suggestion that does not
// exist is deliberately not a fourth value — it is a `404` or an absence from
// the listing — so this file cannot confuse "gone stale" with "never was", and
// neither can the screen.
//
// # "Deliberate by construction — an explicit call, never a default, never a
// batch" (`CONTEXT.md`)
//
// That sentence is a specification for this screen, and it is enforced here:
//
//   - [`actionsFor`] returns the actions of **one** row. There is no function
//     in this module that takes a list, and there is not going to be one:
//     "approve all" would have to be written here first.
//   - nothing is selected by default. A row carries actions, never a
//     "selected" flag, so there is no state a keystroke could approve.
//   - `approve` and `edit` are two actions, not one control with a mode.
//     Editing opens a field; approving the edit is a second, separate press.
//
// # What a row cannot say, and why (#160)
//
// It cannot name the contact. `GET /api/suggestions` carries the trigger as an
// id and a type and nothing else, because naming the contact means opening
// their message and re-publishing a revoked contact's words through a new door
// — the leak #110 closed (ADR 0012). So [`triggerKey`] resolves to *"a reply
// to a WhatsApp message"* and never to *"a reply to Aïcha"*.
//
// This module does not work around that, and the way it does not is worth
// stating: there is no import of `$lib/api/client` here, no second read, and
// no member on `Row` for a display name. The open question is #160's and the
// owner's; the screen says out loud that it cannot name the person, which is
// the honest rendering of a constraint rather than a gap the user discovers.

import type { components } from '$lib/api/schema';
import type { MessageKey } from '$lib/i18n';

export type Suggestion = components['schemas']['Suggestion'];
export type Listing = components['schemas']['SuggestionListing'];
export type Approval = components['schemas']['Approval'];
export type Standing = Suggestion['standing'];
export type Delivery = components['schemas']['Delivery'];
export type Posted = components['schemas']['PostedReport'];

/**
 * One action a row may offer. Never plural, never defaulted.
 *
 * `retry` is not a synonym for `approve`: it is the repair for an approval the
 * Gateway recorded and the bus never acknowledged, which republishes under the
 * same deterministic id and is deduplicated rather than sent twice.
 */
export type Action = 'approve' | 'edit' | 'dismiss' | 'retry';

export interface Row {
	id: string;
	personaId: string;
	network: components['schemas']['Network'];
	producedAt: string;
	expiresAt: string | null;
	attempt: number | null;
	/** The persona's own words: the thing being approved, and the only text here. */
	body: string;
	/**
	 * What the persona says it is answering (#334, #335): who wrote, and two
	 * sentences about what they asked — written by the persona, which is the
	 * only component allowed to read the message. `null` when the suggestion
	 * carries none, and when the label its trigger carried was not granted.
	 * Never the contact's own words: those are reached by opening the message
	 * itself, which is a deliberate click and not a line on a card (#336).
	 */
	context: Suggestion['context'];
	/**
	 * What the draft did before it was written (#367), oldest first: the
	 * governed reads it made of the user's own calendar, and the questions it
	 * put to them in their channel.
	 *
	 * Why a card carries it: a draft that read a calendar, asked a question
	 * and then wrote is more useful than one that guessed, and less
	 * transparent — the user approves an outcome whose path they did not see.
	 * Showing the path makes the approval cover the path as well as the text.
	 *
	 * Never what a step learned. A read's intervals are counted, not kept; a
	 * question's answer is in the channel where the user wrote it. And never
	 * the contact's words, for the reason `context` is not them either.
	 */
	path: Suggestion['path'];
	format: string;
	/**
	 * The sentence the reply will disclose itself with, after the body and on
	 * a line of its own, in the language the persona wrote in (#121, ADR
	 * 0019, ADR 0031) — or `null` when the suggestion carries none. It is
	 * never part of `body`: the body is what the user edits, and this is the
	 * one line on the outgoing message they cannot, because removing it from a
	 * single reply is exactly what ADR 0019 rules out. Whether the Gateway
	 * appends it at approval is the switch's business, which the screen reads
	 * separately; a row does not know.
	 */
	disclosure: string | null;
	/** The message being answered, by identity alone. No sender, no excerpt. */
	trigger: components['schemas']['SuggestionTrigger'];
	standing: Standing;
	approval: Approval | null;
	/**
	 * An approval this Gateway recorded whose reply never reached the bus.
	 *
	 * This is #100's "lost reply", in the one form this origin can actually
	 * see: `publication: "unpublished"` means the row was written and the
	 * publication did not land. Both values of `publication` are terminal and
	 * neither is "in flight", so a screen rendering this as a spinner would be
	 * rendering a state that cannot end.
	 */
	lostReply: boolean;
	/**
	 * Whether a reply could reach the contact, decided by the Gateway
	 * **before** the approval from where the owner's own account stands in
	 * the trigger's room (#216). `cannot_reach` is a certainty: the room is a
	 * bridge's portal and the owner's account is not in it, so a bridge
	 * relays nothing posted there — approving would publish a reply nobody
	 * receives. The screen says so above the button, not after it.
	 */
	delivery: Delivery;
	/**
	 * What the Sensor said the approved reply reached once it posted it, or
	 * `null` while it has said nothing. Never the same fact as
	 * `approval.publication`: published on the bus and delivered to the
	 * contact are two sentences on this screen, by construction.
	 */
	posted: Posted | null;
	/**
	 * What the component that had to send it said when it gave up (#311),
	 * or `null` while nothing has been given up on. A third state, not the
	 * absence of the second: until this existed, a reply that could never
	 * be sent read as "published" for ever, and an owner was told "sent"
	 * for a message that never left.
	 */
	givenUp: Suggestion['given_up'];
	actions: Action[];
}

/**
 * What one row may offer. One suggestion in, one row's actions out.
 *
 * `expired` keeps `dismiss` and loses `approve`: the ticket asks for an old
 * suggestion to be *shown as no longer approvable, with the reason*, because
 * nothing is persisted and making it vanish would be the screen lying about
 * what happened to it.
 */
export function actionsFor(suggestion: Suggestion): Action[] {
	switch (suggestion.standing) {
		case 'approvable':
			return ['approve', 'edit', 'dismiss'];
		case 'expired':
			return ['dismiss'];
		case 'approved':
			return suggestion.approval?.publication === 'unpublished' ? ['retry'] : [];
		default:
			// A standing this build does not know. Offering nothing is the safe
			// answer: an approval is an explicit act, and this is not one.
			return [];
	}
}

export function toRow(suggestion: Suggestion): Row {
	return {
		id: suggestion.event_id,
		personaId: suggestion.persona_id,
		network: suggestion.network,
		producedAt: suggestion.produced_at,
		expiresAt: suggestion.expires_at,
		attempt: suggestion.attempt,
		body: suggestion.suggestion.body,
		format: suggestion.suggestion.format,
		context: suggestion.context,
		path: suggestion.path ?? [],
		disclosure: suggestion.disclosure,
		trigger: suggestion.trigger,
		standing: suggestion.standing,
		approval: suggestion.approval,
		lostReply:
			suggestion.standing === 'approved' && suggestion.approval?.publication === 'unpublished',
		delivery: suggestion.delivery,
		posted: suggestion.posted,
		givenUp: suggestion.given_up,
		actions: actionsFor(suggestion)
	};
}

/**
 * The sentence about delivery that stands **before** the approval button.
 *
 * Three sentences for the Gateway's three answers, and the one for
 * `cannot_reach` names #123 as what would change it: a reader has to know this
 * is a known gap in the mechanism and not a bug in their setup. `warns` is
 * whether the screen draws it as a warning — only the certainty is.
 */
export function deliveryCopy(delivery: Delivery): { key: MessageKey; warns: boolean } {
	switch (delivery.reach) {
		case 'can_reach':
			return { key: 'approvals.delivery.canReach', warns: false };
		case 'cannot_reach':
			return { key: 'approvals.delivery.cannotReach', warns: true };
		default:
			return { key: 'approvals.delivery.unknown', warns: false };
	}
}

/** The word behind the Gateway's answer, as a sentence fragment. */
export function deliveryDetailKey(delivery: Delivery): MessageKey {
	return `approvals.delivery.detail.${delivery.detail}` as MessageKey;
}

/**
 * The sentence about delivery that stands **after** the approval, once the
 * Gateway says the reply is published.
 *
 * "Published on your bus" is the approval's own sentence. This one is about
 * what happened next, and it is the Sensor's report when there is one —
 * `contact` or `nobody`, and by which account — and, while there is none,
 * either the certainty the screen already had (`cannot_reach`) or the honest
 * "not yet". It is never derived from `approval.publication`.
 */
/**
 * Whether the component that had to send this reply gave up on it (#311).
 * One predicate, because the screen asks it three times — the sentence, the
 * warning card, the icon — and three spellings of the same question drift.
 */
export function gaveUp(row: Pick<Row, 'givenUp'>): boolean {
	return row.givenUp !== null && row.givenUp !== undefined;
}

export function postedCopy(row: Pick<Row, 'delivery' | 'posted' | 'givenUp'>): MessageKey {
	// What was given up on is read first (#311). One approval has exactly
	// one sender — the Sensor takes the Matrix connections and the collector
	// the mail ones, each acking the other's untouched — so a reply cannot
	// today be both posted and given up on. The order is stated anyway,
	// because if the two ever did arrive together, "it went nowhere" is the
	// one a person deciding whether to send again has to read.
	if (gaveUp(row)) {
		// A sender older than #311 set no reason, and the Gateway says so
		// with a null rather than inventing English prose for a screen that
		// speaks five languages. The sentence is this catalogue's to write.
		return row.givenUp?.reason
			? 'approvals.posted.givenUp'
			: 'approvals.posted.givenUp.unexplained';
	}
	if (row.posted !== null) {
		return row.posted.reach === 'contact'
			? 'approvals.posted.contact'
			: 'approvals.posted.nobody';
	}
	return row.delivery.reach === 'cannot_reach'
		? 'approvals.delivery.cannotReach'
		: 'approvals.posted.pending';
}

/**
 * The listing, newest first, with the ones this browser has dismissed left
 * out.
 *
 * A dismissal is local (`./dismissed.ts`) and it is not a state the Gateway
 * knows: filtering here rather than at the Gateway is the honest shape,
 * because there is no API that could be asked.
 */
export function toRows(listing: Listing, dismissed: ReadonlySet<string>): Row[] {
	return listing.suggestions.filter((entry) => !dismissed.has(entry.event_id)).map(toRow);
}

/**
 * What the screen can honestly say about the message a suggestion answers.
 *
 * The event type and the network, and nothing else — see the module note. An
 * unknown type still gets a sentence rather than a blank, because a row with
 * no explanation of what it is answering is a row the user cannot judge at
 * all.
 */
export function triggerKey(eventType: string): MessageKey {
	return eventType === 'fr.linagora.twalk.inbound.message.received.v1'
		? 'approvals.trigger.message'
		: 'approvals.trigger.other';
}

/**
 * Whether a suggestion the Gateway called approvable has gone stale since the
 * read.
 *
 * The Gateway decides `standing` at the moment it answers, and a screen left
 * open outlives that answer. This does **not** change what the row offers —
 * the Gateway is the authority on whether an approval is refused, and a client
 * that hid the button would be a second, disagreeing authority — it adds a
 * warning, so that pressing approve and meeting `409 suggestion_expired` is
 * not a surprise.
 */
export function goneStale(row: Row, now: number): boolean {
	if (row.standing !== 'approvable' || row.expiresAt === null) {
		return false;
	}
	const at = Date.parse(row.expiresAt);
	return Number.isFinite(at) && at <= now;
}

/** Something true about the read itself, which the screen says out loud. */
export type Notice =
	/** The bounded read did not reach the stream's first retained message. */
	| { kind: 'window'; count?: undefined }
	/** `limit` cut the list: more were found in the window than answered with. */
	| { kind: 'truncated'; count?: undefined }
	/** Suggestions found and not understood by this Gateway build. */
	| { kind: 'unreadable'; count: number }
	/** Rows this browser dismissed, so the count on screen is not the count read. */
	| { kind: 'dismissed'; count: number };

/**
 * The facts about the read that a user has to be told.
 *
 * `window.reached_start_of_stream` is the one that matters most: the Gateway
 * keeps no copy of a suggestion, so a read that stopped short is a read that
 * may have missed older ones — *"a bound nobody can see is a bound that
 * lies"*. Leaving it in the response and out of the screen would put the lie
 * back.
 */
export function noticesFor(listing: Listing, dismissed: number): Notice[] {
	const notices: Notice[] = [];
	if (!listing.window.reached_start_of_stream) {
		notices.push({ kind: 'window' });
	}
	if (listing.truncated) {
		notices.push({ kind: 'truncated' });
	}
	if (listing.unreadable > 0) {
		notices.push({ kind: 'unreadable', count: listing.unreadable });
	}
	if (dismissed > 0) {
		notices.push({ kind: 'dismissed', count: dismissed });
	}
	return notices;
}

/**
 * How many rows in a listing are waiting for the user — what the dashboard's
 * chip counts.
 *
 * A lost reply counts too: it is a thing the user has to act on, and the
 * dashboard's job is to say how much is waiting rather than to sort it.
 */
export function waitingCount(listing: Listing, dismissed: ReadonlySet<string>): number {
	return listing.suggestions.filter(
		(entry) =>
			!dismissed.has(entry.event_id) &&
			(entry.standing === 'approvable' || entry.approval?.publication === 'unpublished')
	).length;
}
