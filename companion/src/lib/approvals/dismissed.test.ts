// The local half of a refusal: what is stored, and what happens when the
// browser will not store it.
//
// The parsing is the part worth testing directly. A dismissal set read out of
// `localStorage` is a value some other version of this app — or some other
// tab, or a half-finished write — put there, so it is never trusted.

import { describe, expect, it } from 'vitest';

import { parse, serialise } from './dismissed';

describe('reading what was stored', () => {
	it('reads a list of ids', () => {
		expect([...parse('["a","b"]')]).toEqual(['a', 'b']);
	});

	it('reads nothing as nothing', () => {
		expect(parse(null).size).toBe(0);
	});

	it('refuses to fail on someone else’s value', () => {
		// Every one of these shows every suggestion, which is the failure that
		// loses nothing: a row the user refused reappears, and they refuse it
		// again.
		expect(parse('not json').size).toBe(0);
		expect(parse('{"a":1}').size).toBe(0);
		expect(parse('["a",7,null]').size).toBe(1);
	});
});

describe('writing', () => {
	it('keeps the newest and drops the oldest past the lid', () => {
		const many = Array.from({ length: 250 }, (_, index) => `id-${index}`);
		const kept = JSON.parse(serialise(many)) as string[];
		expect(kept.length).toBe(200);
		expect(kept[kept.length - 1]).toBe('id-249');
		expect(kept).not.toContain('id-0');
	});

	it('round-trips through parse', () => {
		expect([...parse(serialise(['x', 'y']))]).toEqual(['x', 'y']);
	});
});
