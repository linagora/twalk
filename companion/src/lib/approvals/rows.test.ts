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
	deliveryCopy,
	deliveryDetailKey,
	goneStale,
	noticesFor,
	postedCopy,
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
		disclosure: 'Rédigé avec mon assistant IA.',
		stream_sequence: 42,
		approval: null,
		delivery: { reach: 'unknown', detail: 'not_a_known_portal' },
		posted: null,
		undelivered: null,
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

describe('published is not delivered (#216)', () => {
	it('says before the button whether the reply can reach the contact, and why', () => {
		// The certainty is the one drawn as a warning: the owner's account is
		// not in the portal, so a bridge relays nothing posted there.
		const cannot = toRow(
			suggestion({ delivery: { reach: 'cannot_reach', detail: 'owner_invited' } })
		);
		expect(deliveryCopy(cannot.delivery)).toEqual({
			key: 'approvals.delivery.cannotReach',
			warns: true
		});
		expect(deliveryDetailKey(cannot.delivery)).toBe('approvals.delivery.detail.owner_invited');
		// And the button stays: the Gateway is the authority on refusing.
		expect(cannot.actions).toContain('approve');

		const can = toRow(suggestion({ delivery: { reach: 'can_reach', detail: 'owner_joined' } }));
		expect(deliveryCopy(can.delivery)).toEqual({ key: 'approvals.delivery.canReach', warns: false });

		const unknown = toRow(suggestion());
		expect(deliveryCopy(unknown.delivery)).toEqual({
			key: 'approvals.delivery.unknown',
			warns: false
		});
	});

	it('says a reply did not go out, even when something else was reported', () => {
		// #311: three states, not two. A reply given up on used to read as
		// "published" for ever, and an owner was told "sent" for a message
		// that never left — three times in one morning, on a real
		// deployment, before this sentence existed.
		const givenUp = toRow(
			suggestion({
				standing: 'approved',
				approval: approval(),
				undelivered: { reason: 'the JMAP server answered unknownMethod', stream_sequence: 91 }
			})
		);
		expect(postedCopy(givenUp)).toBe('approvals.posted.undelivered');

		// And it wins over a report: the Sensor may have reached a room
		// while the collector gave up on the mail half, and "it went
		// nowhere" is what someone deciding whether to send again needs.
		const bothSaid = toRow(
			suggestion({
				standing: 'approved',
				approval: approval(),
				posted: { reach: 'contact', posted_as: 'mailto:michel@example.com', stream_sequence: 90 },
				undelivered: { reason: 'exhausted its retries', stream_sequence: 91 }
			})
		);
		expect(postedCopy(bothSaid)).toBe('approvals.posted.undelivered');

		// And a sender that said nothing still gets a sentence, in this
		// catalogue's words rather than the Gateway's: a Sensor older than
		// #311 published dead letters with no reason header at all.
		const unexplained = toRow(
			suggestion({
				standing: 'approved',
				approval: approval(),
				undelivered: { reason: null, stream_sequence: 91 }
			})
		);
		expect(postedCopy(unexplained)).toBe('approvals.posted.undelivered.unexplained');
	});

	it('never turns the approval record into a delivery sentence', () => {
		// Published, and the Sensor has said nothing yet: the honest "not
		// yet", never "delivered" read off `publication`.
		const published = toRow(suggestion({ standing: 'approved', approval: approval() }));
		expect(published.posted).toBeNull();
		expect(postedCopy(published)).toBe('approvals.posted.pending');

		// Published, and the screen already knew it could not reach anyone:
		// that certainty is what is said, not "not yet".
		const doomed = toRow(
			suggestion({
				standing: 'approved',
				approval: approval(),
				delivery: { reach: 'cannot_reach', detail: 'owner_invited' }
			})
		);
		expect(postedCopy(doomed)).toBe('approvals.delivery.cannotReach');

		// The Sensor's report, when there is one, is the sentence — either way.
		const delivered = toRow(
			suggestion({
				standing: 'approved',
				approval: approval(),
				posted: { reach: 'contact', posted_as: '@owner:test.twalk', stream_sequence: 44 }
			})
		);
		expect(postedCopy(delivered)).toBe('approvals.posted.contact');
		const nobody = toRow(
			suggestion({
				standing: 'approved',
				approval: approval(),
				posted: { reach: 'nobody', posted_as: '@sensor:test.twalk', stream_sequence: 44 }
			})
		);
		expect(postedCopy(nobody)).toBe('approvals.posted.nobody');
	});
});

describe('the disclosure (#121)', () => {
	it('is carried beside the body, and never inside it', () => {
		// ADR 0031: a field of its own on the event, appended by the Gateway at
		// approval. The body stays the persona's words alone — it is what the
		// editor opens on — and the sentence is the one line the user cannot
		// edit, so a row keeps them as two members and nothing here joins them.
		const row = toRow(suggestion());
		expect(row.disclosure).toBe('Rédigé avec mon assistant IA.');
		expect(row.body).toBe('Pas de problème, à 20h !');
		expect(row.body).not.toContain(row.disclosure);
		expect(Object.keys(row)).not.toContain('final');
		expect(Object.keys(row)).not.toContain('outgoing');
	});

	it('is null when the suggestion carries none, and the body is still the body', () => {
		// One published before the member existed, or by a persona that set
		// none: the reply then goes out undisclosed, and the screen says so
		// rather than inventing a sentence the Gateway would not append.
		const row = toRow(suggestion({ disclosure: null }));
		expect(row.disclosure).toBeNull();
		expect(row.body).toBe('Pas de problème, à 20h !');
	});
});
