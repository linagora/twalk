// The settings screen's rules, against values (ticket #101): what the
// credential's three states are, what the form sends and when it refuses to,
// and that every refusal the Gateway documents has a sentence.

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import {
	type CalendarLocationState,
	type DisclosureState,
	type ModelConfiguration,
	PROBE_FAILURE_COPY,
	REFUSAL_COPY,
	calendarLocationRecord,
	credentialState,
	disclosureDate,
	disclosureRecord,
	formOf,
	isWallClock,
	requestOf,
	daysOnTheDefault,
	exceptionDays,
	workingDayBody,
	workingDayForm,
	workingDayRecord,
	workingDayTrouble
} from './model';

function configuration(over: Partial<ModelConfiguration> = {}): ModelConfiguration {
	return {
		configured: true,
		base_url: 'http://127.0.0.1:4000/v1',
		model: 'qwen',
		params: null,
		personas: {},
		updated_at: '2026-09-20T08:00:00Z',
		credential: {
			configured: true,
			source: 'companion',
			hint: 'k3y9',
			file: null,
			companion_credential_stored: true
		},
		...over
	};
}

describe('the credential', () => {
	it('is file-locked when an operator supplied a file, whatever the browser stored', () => {
		expect(
			credentialState(
				configuration({
					credential: {
						configured: true,
						source: 'file',
						hint: null,
						file: '/etc/twalk/llm.key',
						companion_credential_stored: true
					}
				})
			)
		).toEqual({ kind: 'file', path: '/etc/twalk/llm.key', companionStored: true });
	});

	it('is described, never carried, when the browser set it', () => {
		expect(credentialState(configuration())).toEqual({ kind: 'companion', hint: 'k3y9' });
		// And the form it fills has no credential in it to round-trip.
		expect(formOf(configuration()).credential).toBe('');
	});

	it('is none on a fresh deployment, which is a state and not an error', () => {
		const fresh = configuration({
			configured: false,
			base_url: null,
			model: null,
			updated_at: null,
			credential: {
				configured: false,
				source: null,
				hint: null,
				file: null,
				companion_credential_stored: false
			}
		});
		expect(credentialState(fresh)).toEqual({ kind: 'none' });
		expect(formOf(fresh)).toEqual({ baseUrl: '', model: '', credential: '', params: '' });
	});
});

describe('the request the form sends', () => {
	const form = { baseUrl: 'http://127.0.0.1:4000/v1', model: 'qwen', credential: '', params: '' };

	it('leaves the credential out when the field is empty, so a stored one survives a rename', () => {
		const built = requestOf(form);
		expect(built).toEqual({
			ok: true,
			request: { base_url: 'http://127.0.0.1:4000/v1', model: 'qwen', params: null }
		});
		expect(built.ok && 'credential' in built.request).toBe(false);
	});

	it('sends null only as the explicit act of forgetting it, and a string to replace it', () => {
		expect(requestOf(form, { forgetCredential: true })).toMatchObject({
			ok: true,
			request: { credential: null }
		});
		expect(requestOf({ ...form, credential: '  sk-abc ' })).toMatchObject({
			ok: true,
			request: { credential: 'sk-abc' }
		});
	});

	it('carries the provider parameters as an object, and refuses anything else', () => {
		expect(requestOf({ ...form, params: '{"temperature": 0.2, "top_p": null}' })).toMatchObject({
			ok: true,
			request: { params: { temperature: 0.2, top_p: null } }
		});
		expect(requestOf({ ...form, params: '[1, 2]' })).toEqual({
			ok: false,
			problems: [{ field: 'params', because: 'not_an_object' }]
		});
		expect(requestOf({ ...form, params: 'not json' })).toEqual({
			ok: false,
			problems: [{ field: 'params', because: 'not_an_object' }]
		});
	});

	it('names the field that is empty or not a URL, before the Gateway is asked', () => {
		expect(requestOf({ ...form, baseUrl: '' })).toEqual({
			ok: false,
			problems: [{ field: 'baseUrl', because: 'empty' }]
		});
		expect(requestOf({ ...form, baseUrl: '127.0.0.1:4000' })).toEqual({
			ok: false,
			problems: [{ field: 'baseUrl', because: 'not_a_url' }]
		});
		expect(requestOf({ ...form, model: '   ' })).toEqual({
			ok: false,
			problems: [{ field: 'model', because: 'empty' }]
		});
	});
});

describe('the disclosure record (#121)', () => {
	const off: DisclosureState = {
		enabled: false,
		since: '2026-09-20T08:05:00Z',
		actor: '@owner:test.twalk',
		reason: null
	};

	it('is "on" and nobody\'s decision while nobody has decided', () => {
		// The default state answers `null` for `since` and `actor`, and that is
		// a record — the sentence must not read as a decision somebody took.
		expect(
			disclosureRecord({ enabled: true, since: null, actor: null, reason: null }, 'en')
		).toEqual({ kind: 'on', key: 'settings.disclosure.record.on', values: {} });
	});

	it('is "off since <date> by <actor>" once the switch was turned off', () => {
		const record = disclosureRecord(off, 'en');
		expect(record.kind).toBe('off');
		expect(record.key).toBe('settings.disclosure.record.off');
		expect(record.values).toEqual({
			date: disclosureDate('2026-09-20T08:05:00Z', 'en'),
			actor: '@owner:test.twalk'
		});
	});


	it('keeps "turned back on" apart from "never turned off"', () => {
		const record = disclosureRecord({ ...off, enabled: true }, 'fr');
		expect(record.kind).toBe('on');
		expect(record.key).toBe('settings.disclosure.record.onSince');
		expect(record.values).toMatchObject({ actor: '@owner:test.twalk' });
	});

	it('formats the date in the interface\'s locale, from the instant the Gateway stamped', () => {
		// The same instant, two interfaces: what the user reads is a date in
		// their own language, and the journal's row is not translated on the
		// way — only rendered.
		const french = disclosureDate('2026-09-20T08:05:00Z', 'fr');
		const english = disclosureDate('2026-09-20T08:05:00Z', 'en');
		expect(french).toContain('septembre');
		expect(english).toContain('September');
		expect(french).toContain('2026');
		expect(disclosureRecord(off, 'fr').values.date).toBe(french);
	});

	it('renders an instant it cannot parse as itself, never as nothing', () => {
		expect(disclosureDate('not a date', 'en')).toBe('not a date');
	});

	it('reads a false with no date as off, not as on', () => {
		// A Gateway this build does not know answered something the shape
		// forbids; the safe rendering is the state it did say.
		const record = disclosureRecord({ ...off, since: null, actor: null }, 'en');
		expect(record.kind).toBe('off');
		expect(record.values).toEqual({ date: '', actor: '' });
	});
});

describe('every refusal the Gateway describes has a sentence', () => {
	// Read off the description itself, so a code added to the Gateway fails
	// here rather than rendering as a blank — #100's rule for the approval
	// screen, applied to the settings routes.
	const description = readFileSync(
		new URL('../../../../companion-gateway/openapi.yaml', import.meta.url),
		'utf8'
	);

	function enumsIn(section: string, codes: Set<string>): void {
		for (const match of section.matchAll(/enum:\s*\[([^\]]*)\]/g)) {
			for (const code of match[1]?.split(',') ?? []) {
				const trimmed = code.trim();
				if (/^[a-z_]+$/.test(trimmed)) {
					codes.add(trimmed);
				}
			}
		}
		for (const match of section.matchAll(/enum:\s*\n\s*\[([^\]]*)\]/g)) {
			for (const code of match[1]?.split(',') ?? []) {
				const trimmed = code.trim();
				if (/^[a-z_]+$/.test(trimmed)) {
					codes.add(trimmed);
				}
			}
		}
	}

	// The codes under a path, inline and behind a `$ref` to a shared response
	// alike: the disclosure routes answer `consent_not_configured` only through
	// `#/components/responses/DisclosureNotConfigured`, and a walk that stopped
	// at the reference could not prove that code has a sentence.
	function codesUnder(path: string): string[] {
		const start = description.indexOf(`\n  ${path}:\n`);
		expect(start, path).toBeGreaterThan(-1);
		const next = description.indexOf('\n  /', start + 1);
		const section = description.slice(start, next === -1 ? undefined : next);
		const codes = new Set<string>();
		enumsIn(section, codes);
		for (const match of section.matchAll(/\$ref: "#\/components\/responses\/([A-Za-z]+)"/g)) {
			const responseStart = description.indexOf(`\n    ${match[1]}:\n`);
			expect(responseStart, match[1]).toBeGreaterThan(-1);
			const rest = description.slice(responseStart + 1);
			const sibling = rest.search(/\n    [A-Za-z]+:\n/);
			enumsIn(sibling === -1 ? rest : rest.slice(0, sibling), codes);
		}
		return [...codes];
	}

	it('for writing a model, setting a language and switching the disclosure', () => {
		const known = new Set([...Object.keys(REFUSAL_COPY), 'unauthenticated', 'sign_in_not_configured']);
		for (const path of ['/api/settings/model', '/api/settings/language', '/api/settings/disclosure']) {
			for (const code of codesUnder(path)) {
				expect(known.has(code), `${path}: ${code}`).toBe(true);
			}
		}
		// And the walk really reaches behind the references, or the line above
		// proves nothing about the codes the shared responses carry.
		expect(codesUnder('/api/settings/disclosure')).toContain('consent_not_configured');
		expect(codesUnder('/api/settings/model')).toContain('settings_not_configured');
	});

	it('for the probe, whose four answers are four fixes', () => {
		const known = new Set([
			...Object.keys(PROBE_FAILURE_COPY),
			...Object.keys(REFUSAL_COPY),
			'unauthenticated',
			'sign_in_not_configured'
		]);
		for (const code of codesUnder('/api/settings/model/probe')) {
			expect(known.has(code), code).toBe(true);
		}
		expect(Object.keys(PROBE_FAILURE_COPY)).toEqual([
			'model_not_configured',
			'endpoint_unreachable',
			'endpoint_refused',
			'endpoint_not_compatible'
		]);
	});
});

describe('the calendar location record (#354)', () => {
	/** The switch as a decision left it: allowed, dated, attributed. */
	const allowed: CalendarLocationState = {
		enabled: true,
		since: '2026-09-20T08:05:00Z',
		actor: '@owner:test.twalk',
		reason: null
	};

	it('is the disclosure record with its emphasis reversed', () => {
		// The same three shapes with the alarming state the other way round.
		// Off with no date is how a deployment ships — "nobody decided" — and
		// it is the *on* state the card marks, because that is the direction
		// in which a place leaves the machine.
		const shipped = calendarLocationRecord(
			{ enabled: false, since: null, actor: null, reason: null },
			'en'
		);
		expect(shipped).toEqual({
			kind: 'quiet',
			key: 'settings.calendarLocation.record.off',
			values: {}
		});

		const sending = calendarLocationRecord(allowed, 'en');
		expect(sending.kind).toBe('sending');
		expect(sending.key).toBe('settings.calendarLocation.record.on');
		expect(sending.values).toEqual({
			date: disclosureDate('2026-09-20T08:05:00Z', 'en'),
			actor: '@owner:test.twalk'
		});

		// Withheld *again* is dated and attributed, and still quiet: it is
		// the state that sends nothing.
		const withheld = calendarLocationRecord({ ...allowed, enabled: false }, 'en');
		expect(withheld.kind).toBe('quiet');
		expect(withheld.key).toBe('settings.calendarLocation.record.offSince');
	});
});

describe("the owner's working day (#381)", () => {
	const state = (day: unknown, since: string | null = null, actor: string | null = null) =>
		({ day, since, actor, reason: null }) as Parameters<typeof workingDayRecord>[0];

	it('opens on a plausible week when nothing was ever said, without recording one', () => {
		// A form nobody can submit teaches nothing, so it opens filled — but
		// the *state* stays unset until the user presses the button, which is
		// what the record line says.
		expect(workingDayForm(null)).toEqual({
			days: [1, 2, 3, 4, 5],
			startsAt: '09:00',
			endsAt: '18:00',
			exceptions: {}
		});
		expect(workingDayRecord(state(null), 'fr').key).toBe('settings.workingDay.record.unset');
	});

	it('reads a state back into the form, and says who set it', () => {
		const set = state({ days: [2, 4], starts_at: '10:00', ends_at: '16:30' }, '2026-09-26T15:12:04.000Z', '@michel:twalk.localhost');
		expect(workingDayForm(set)).toEqual({
			days: [2, 4],
			startsAt: '10:00',
			endsAt: '16:30',
			exceptions: {}
		});
		const record = workingDayRecord(set, 'fr');
		expect(record.kind).toBe('set');
		expect(record.key).toBe('settings.workingDay.record.set');
		expect(record.values).toMatchObject({ actor: '@michel:twalk.localhost' });
	});

	it('tells a clearing from nobody ever having said', () => {
		// Both offer every gap; only one of them was a decision, and dating a
		// default would invent one.
		const cleared = state(null, '2026-09-26T16:00:00.000Z', '@michel:twalk.localhost');
		expect(workingDayRecord(cleared, 'fr').key).toBe('settings.workingDay.record.cleared');
		expect(workingDayRecord(state(null), 'fr').key).toBe('settings.workingDay.record.unset');
	});

	it('says why a form cannot be sent, rather than leaving it to a 422', () => {
		const form = { days: [1], startsAt: '09:00', endsAt: '18:00', exceptions: {} };
		expect(workingDayTrouble(form)).toBeNull();
		expect(workingDayTrouble({ ...form, days: [] })).toBe('days');
		expect(workingDayTrouble({ ...form, startsAt: '9:00' })).toBe('time');
		// An amplitude that ends before it begins is a night, and a working
		// day that wraps midnight is a different thing.
		expect(workingDayTrouble({ ...form, startsAt: '18:00', endsAt: '09:00' })).toBe('order');
		expect(workingDayTrouble({ ...form, startsAt: '09:00', endsAt: '09:00' })).toBe('order');
	});

	it('carries the days that run other hours, and only for days still ticked', () => {
		// #386: a short Wednesday, said without making the whole week as narrow
		// as it.
		const form = workingDayForm({
			day: {
				days: [1, 2, 3, 4, 5],
				starts_at: '09:00',
				ends_at: '18:30',
				exceptions: { '3': { starts_at: '09:00', ends_at: '12:30' } }
			},
			since: '2026-09-26T20:00:00.000Z',
			actor: '@owner:example.com',
			reason: null
		});
		expect(exceptionDays(form)).toEqual([3]);
		expect(daysOnTheDefault(form)).toEqual([1, 2, 4, 5]);
		expect(workingDayBody(form)).toEqual({
			days: [1, 2, 3, 4, 5],
			starts_at: '09:00',
			ends_at: '18:30',
			exceptions: { '3': { starts_at: '09:00', ends_at: '12:30' } }
		});

		// A day no longer ticked takes its hours with it: the Gateway refuses an
		// exception for a day the user does not accept, and the form must not
		// send a contradiction it can see.
		const untickedWednesday = { ...form, days: [1, 2, 4, 5] };
		expect(exceptionDays(untickedWednesday)).toEqual([]);
		expect(workingDayBody(untickedWednesday)).toEqual({
			days: [1, 2, 4, 5],
			starts_at: '09:00',
			ends_at: '18:30'
		});

		// And a week with one amplitude sends the body it sent before #386 —
		// no member where there was none.
		expect(workingDayBody({ ...form, exceptions: {} })).toEqual({
			days: [1, 2, 3, 4, 5],
			starts_at: '09:00',
			ends_at: '18:30'
		});
	});

	it('says why an exception cannot be sent, as it does for the day itself', () => {
		const form = {
			days: [1, 3],
			startsAt: '09:00',
			endsAt: '18:00',
			exceptions: { 3: { startsAt: '09:00', endsAt: '12:30' } }
		};
		expect(workingDayTrouble(form)).toBeNull();
		expect(
			workingDayTrouble({ ...form, exceptions: { 3: { startsAt: '9:00', endsAt: '12:30' } } })
		).toBe('time');
		expect(
			workingDayTrouble({ ...form, exceptions: { 3: { startsAt: '12:30', endsAt: '09:00' } } })
		).toBe('order');
		// An exception for a day that is not ticked is not the form's to
		// report: it is dropped before it is sent, so there is nothing to say.
		expect(
			workingDayTrouble({ ...form, days: [1], exceptions: { 3: { startsAt: '25:00', endsAt: '26:00' } } })
		).toBeNull();
	});

	it('accepts only HH:MM on a 24-hour clock, the spelling the Gateway takes', () => {
		for (const good of ['00:00', '09:05', '18:30', '23:59']) {
			expect(isWallClock(good)).toBe(true);
		}
		for (const bad of ['9:05', '09:5', '24:00', '23:60', '0900', '09:00:00', '', 'ab:cd']) {
			expect(isWallClock(bad)).toBe(false);
		}
	});
});
