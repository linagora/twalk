// Reading the Google cookies out of whatever the user managed to copy.
//
// This is the part of screen 3c that decides between "connected" and a refusal
// the user cannot act on, so every spelling they will plausibly arrive with is
// covered — and so is the rule that a value is never cleaned up, since a Google
// session cookie is full of characters that look like punctuation.

import { describe, expect, it } from 'vitest';

import { missing, parseCookies } from './cookies';

/** The shape of a real one: slashes, dashes, underscores and a trailing `=`. */
const SID = 'g.a000xQhF-7bK_1nN/2mVzQaB3cD4eF5gH6iJ7kL8mN9oP0qR1sT2uV3wX4yZ5a==';

describe('parsing pasted cookies', () => {
	it('reads what document.cookie and a Cookie header look like', () => {
		const parsed = parseCookies(`SID=${SID}; HSID=AabbCC; SSID=DdeeFF`);
		expect(parsed).toEqual({ ok: true, cookies: { SID, HSID: 'AabbCC', SSID: 'DdeeFF' } });
	});

	it('keeps the value exactly, punctuation and all', () => {
		// "Cleaning" a value is how a login fails with no visible cause.
		const parsed = parseCookies(`SID=${SID}`);
		expect(parsed.ok && parsed.cookies['SID']).toBe(SID);
	});

	it('strips a Cookie: prefix pasted from the Network tab', () => {
		const parsed = parseCookies('Cookie: SID=abc; HSID=def');
		expect(parsed.ok && parsed.cookies['SID']).toBe('abc');
	});

	it('reads one pair per line, which is what a table copy gives', () => {
		const parsed = parseCookies('SID=abc\nHSID=def\r\nSSID=ghi');
		expect(parsed.ok && Object.keys(parsed.cookies)).toEqual(['SID', 'HSID', 'SSID']);
	});

	it('reads a JSON object', () => {
		const parsed = parseCookies('{"SID": "abc", "HSID": "def"}');
		expect(parsed).toEqual({ ok: true, cookies: { SID: 'abc', HSID: 'def' } });
	});

	it('reads a cookie extension’s export', () => {
		const parsed = parseCookies(
			JSON.stringify([
				{ name: 'SID', value: 'abc', domain: '.google.com' },
				{ name: 'HSID', value: 'def' },
				{ note: 'not a cookie' }
			])
		);
		expect(parsed).toEqual({ ok: true, cookies: { SID: 'abc', HSID: 'def' } });
	});

	it('separates “you pasted nothing” from “that is not cookies”', () => {
		// Two different sentences on screen, because they need two different
		// actions from the user.
		expect(parseCookies('   ')).toEqual({ ok: false, reason: 'empty' });
		expect(parseCookies('I could not find them')).toEqual({ ok: false, reason: 'unreadable' });
		expect(parseCookies('{not json')).toEqual({ ok: false, reason: 'unreadable' });
		expect(parseCookies('[]')).toEqual({ ok: false, reason: 'unreadable' });
	});
});

describe('the cookies the bridge asked for', () => {
	it('names the ones that are absent, rather than saying “invalid”', () => {
		const wanted = ['SID', 'HSID', 'SSID', 'APISID', 'SAPISID', '__Secure-1PSID', '__Secure-1PSIDTS'];
		const parsed = parseCookies('SID=a; HSID=b; SSID=c; APISID=d; SAPISID=e');
		expect(parsed.ok && missing(wanted, parsed.cookies)).toEqual([
			'__Secure-1PSID',
			'__Secure-1PSIDTS'
		]);
	});

	it('treats an empty value as absent', () => {
		expect(missing(['SID'], { SID: '' })).toEqual(['SID']);
		expect(missing(['SID'], { SID: 'a' })).toEqual([]);
	});
});
