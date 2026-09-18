// The seam the dashboard's promise rests on.
//
// #100 asks for a journey that greps the rendered dashboard for a suggestion's
// text and finds nothing. That journey proves the pixels; this test proves the
// *value*, which is the thing that cannot drift: a component can stop
// rendering something and start again, and a value that never carried the text
// cannot.

import { describe, expect, it } from 'vitest';

import { summarise } from './summary';
import type { Listing, Suggestion } from './rows';

const BODY = 'On se retrouve à 20h devant le théâtre, ça te va ?';

function listing(over: Partial<Listing> = {}): Listing {
	const one = {
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
			event_id: 'a'.repeat(64),
			event_type: 'fr.linagora.twalk.inbound.message.received.v1'
		},
		suggestion: { body: BODY, format: 'text/plain' },
		stream_sequence: 42,
		approval: null
	} as Suggestion;
	return {
		suggestions: [one],
		window: { from_sequence: 1, to_sequence: 100, sequences: 1000, reached_start_of_stream: true },
		truncated: false,
		unreadable: 0,
		...over
	} as Listing;
}

describe('the dashboard summary', () => {
	it('carries not one word of what a persona wrote', () => {
		const summary = summarise(listing(), new Set());
		expect(JSON.stringify(summary)).not.toContain('20h');
		expect(JSON.stringify(summary)).not.toContain(BODY);
		expect(Object.keys(summary).sort()).toEqual(['atLeast', 'count']);
	});

	it('counts what is waiting', () => {
		expect(summarise(listing(), new Set()).count).toBe(1);
	});

	it('says when the count is a floor rather than a total', () => {
		// A chip reading "12" when it means "at least 12" is the Gateway's
		// visible bound going invisible one screen further out.
		expect(summarise(listing({ truncated: true }), new Set()).atLeast).toBe(true);
		expect(summarise(listing(), new Set()).atLeast).toBe(false);
	});

	it('leaves out what this browser refused', () => {
		expect(summarise(listing(), new Set(['b'.repeat(64)])).count).toBe(0);
	});
});
