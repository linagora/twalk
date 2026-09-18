// The row model: which actions a suggestion may offer, and what the screen is
// told about the read it came from.
//
// The assertions worth reading are the ones about *absence*: no action on an
// expired row, no function that takes a list, and nothing in a row that names
// the contact. Twalk's promises are only real because a test looks for the
// thing (`CONTRIBUTING.md`).

import { describe, expect, it } from 'vitest';

import {
	actionsFor,
	goneStale,
	noticesFor,
	toRow,
	toRows,
	triggerKey,
	waitingCount,
	type Listing,
	type Suggestion
} from './rows';

const TRIGGER_ID = 'a'.repeat(64);

function suggestion(over: Partial<Suggestion> = {}): Suggestion {
	return {
		event_id: 'b'.repeat(64),
		source: 'hermes://twalk.example.com/personas/assistant',
		persona_id: 'assistant',
		network: 'whatsapp',
		consent: 'granted',
		produced_at: '2026-09-18T08:00:00Z',
		expires_at: '2026-09-18T09:00:00Z',
		attempt: 1,
		standing: 'approvable',
		trigger: {
			event_id: TRIGGER_ID,
			event_type: 'fr.linagora.twalk.inbound.message.received.v1'
		},
		suggestion: { body: 'Pas de problème, à 20h !', format: 'text/plain' },
		stream_sequence: 42,
		approval: null,
		...over
	} as Suggestion;
}

function approval(over: Record<string, unknown> = {}) {
	return {
		event_id: 'c'.repeat(64),
		suggestion_event_id: 'b'.repeat(64),
		approved_by: '@owner:test.twalk',
		persona_id: 'assistant',
		network: 'whatsapp',
		contact: '@whatsapp_33612345678:test.twalk',
		edited: false,
		approved_at: '2026-09-18T08:05:00Z',
		publication: 'published',
		stream_sequence: 43,
		published_at: '2026-09-18T08:05:00Z',
		...over
	} as Suggestion['approval'];
}

function listing(over: Partial<Listing> = {}): Listing {
	return {
		suggestions: [suggestion()],
		window: {
			from_sequence: 1,
			to_sequence: 100,
			sequences: 1000,
			reached_start_of_stream: true
		},
		truncated: false,
		unreadable: 0,
		...over
	} as Listing;
}

describe('three situations, three answers', () => {
	it('offers approve, edit and refuse on an approvable suggestion', () => {
		expect(actionsFor(suggestion({ standing: 'approvable' }))).toEqual([
			'approve',
			'edit',
			'dismiss'
		]);
	});

	it('offers no approval on an expired one, and still shows it', () => {
		// "nothing is persisted, so an old one genuinely cannot be acted on,
		// and saying so beats making it vanish" (#100).
		const actions = actionsFor(suggestion({ standing: 'expired' }));
		expect(actions).not.toContain('approve');
		expect(actions).not.toContain('retry');
		expect(toRow(suggestion({ standing: 'expired' })).body.length).toBeGreaterThan(0);
	});

	it('offers nothing on one that was approved and published', () => {
		expect(actionsFor(suggestion({ standing: 'approved', approval: approval() }))).toEqual([]);
	});

	it('offers nothing at all for a standing this build does not know', () => {
		// Fail closed: an approval is an explicit act, and a row this version
		// cannot read is not one it may offer to send.
		expect(actionsFor(suggestion({ standing: 'brand-new' as Suggestion['standing'] }))).toEqual(
			[]
		);
	});
});

describe('a lost reply', () => {
	it('is an approval this Gateway recorded whose reply never reached the bus', () => {
		const row = toRow(
			suggestion({
				standing: 'approved',
				approval: approval({ publication: 'unpublished', stream_sequence: null })
			})
		);
		expect(row.lostReply).toBe(true);
		// Its text is here, which is what #100 asks for: the dashboard shows the
		// fact, this screen shows the reply and the way to send it.
		expect(row.body).toContain('20h');
		expect(row.actions).toEqual(['retry']);
	});

	it('is not a published approval', () => {
		expect(toRow(suggestion({ standing: 'approved', approval: approval() })).lostReply).toBe(
			false
		);
	});
});

describe('never a batch, never a default', () => {
	it('decides actions one suggestion at a time', () => {
		// `actionsFor` takes a suggestion, not a list. This is the property
		// `CONTEXT.md` calls "deliberate by construction", asserted at the one
		// place an "approve all" would have to be written.
		expect(actionsFor.length).toBe(1);
	});

	it('marks nothing as chosen', () => {
		const row = toRow(suggestion());
		expect(Object.keys(row)).not.toContain('selected');
		expect(Object.keys(row)).not.toContain('checked');
	});
});

describe('what a row may say about the message it answers', () => {
	it('is the network and the kind of event, and nothing else', () => {
		expect(triggerKey('fr.linagora.twalk.inbound.message.received.v1')).toBe(
			'approvals.trigger.message'
		);
		expect(triggerKey('fr.linagora.twalk.inbound.reaction.added.v1')).toBe(
			'approvals.trigger.other'
		);
	});

	it('carries no sender, no display name and no excerpt (#110, #160)', () => {
		// The Gateway's projection never opens an inbound event, and this
		// module never asks a second question. A row that had grown a place to
		// put a contact would be the first half of the leak coming back.
		const row = toRow(suggestion());
		expect(JSON.stringify(row)).not.toContain('contact');
		expect(Object.keys(row.trigger).sort()).toEqual(['event_id', 'event_type']);
	});
});

describe('the bound on the read', () => {
	it('is said out loud when it was reached', () => {
		const notices = noticesFor(
			listing({
				window: {
					from_sequence: 50,
					to_sequence: 100,
					sequences: 50,
					reached_start_of_stream: false
				}
			}),
			0
		);
		expect(notices.map((notice) => notice.kind)).toContain('window');
	});

	it('says nothing when the whole stream was read', () => {
		expect(noticesFor(listing(), 0)).toEqual([]);
	});

	it('keeps a truncated page apart from a bounded stream', () => {
		// One is about the request, the other about the bus: the Gateway's own
		// description makes the distinction, so the screen keeps it.
		const notices = noticesFor(listing({ truncated: true }), 0);
		expect(notices.map((notice) => notice.kind)).toEqual(['truncated']);
	});

	it('counts what this build could not read rather than dropping it', () => {
		const notices = noticesFor(listing({ unreadable: 3 }), 0);
		expect(notices).toContainEqual({ kind: 'unreadable', count: 3 });
	});

	it('says how many rows this browser is hiding', () => {
		expect(noticesFor(listing(), 2)).toContainEqual({ kind: 'dismissed', count: 2 });
	});
});

describe('dismissal', () => {
	it('leaves a refused row out of the list', () => {
		const rows = toRows(listing(), new Set(['b'.repeat(64)]));
		expect(rows).toEqual([]);
	});

	it('leaves the others alone', () => {
		expect(toRows(listing(), new Set(['z'.repeat(64)])).length).toBe(1);
	});
});

describe('staleness while the screen is open', () => {
	const at = Date.parse('2026-09-18T09:00:00Z');

	it('warns once the expiry has passed', () => {
		expect(goneStale(toRow(suggestion()), at + 1)).toBe(true);
	});

	it('says nothing before it', () => {
		expect(goneStale(toRow(suggestion()), at - 1)).toBe(false);
	});

	it('says nothing about a suggestion with no expiry', () => {
		// The contract allows it and the first-party SDK never produces it.
		expect(goneStale(toRow(suggestion({ expires_at: null })), at + 1)).toBe(false);
	});

	it('says nothing about one the Gateway already called expired', () => {
		// That row renders its own reason; two messages for one fact is the
		// noise this screen is trying not to make.
		expect(goneStale(toRow(suggestion({ standing: 'expired' })), at + 1)).toBe(false);
	});
});

describe('what is waiting for the user', () => {
	it('counts approvable rows and lost replies, and nothing else', () => {
		const dismissedNone = new Set<string>();
		expect(waitingCount(listing(), dismissedNone)).toBe(1);
		expect(
			waitingCount(
				listing({ suggestions: [suggestion({ standing: 'expired' })] }),
				dismissedNone
			)
		).toBe(0);
		expect(
			waitingCount(
				listing({ suggestions: [suggestion({ standing: 'approved', approval: approval() })] }),
				dismissedNone
			)
		).toBe(0);
		expect(
			waitingCount(
				listing({
					suggestions: [
						suggestion({
							standing: 'approved',
							approval: approval({ publication: 'unpublished' })
						})
					]
				}),
				dismissedNone
			)
		).toBe(1);
	});

	it('does not count what this browser refused', () => {
		expect(waitingCount(listing(), new Set(['b'.repeat(64)]))).toBe(0);
	});
});
