// The things about i18n that are logic rather than copy: which locale the
// browser's preferences resolve to, whether the five catalogues say the same
// things, and whether their ICU patterns actually format.

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import de from './de.json';
import en from './en.json';
import es from './es.json';
import fr from './fr.json';
import it_ from './it.json';
import { LOCALES, pickLocale, type Locale } from './locale';
import { translate } from './index';

const catalogues: Record<Locale, Record<string, string>> = { en, fr, it: it_, es, de };

/** The placeholder names an ICU pattern interpolates, in order of first use. */
function placeholders(pattern: string): string[] {
	return [...new Set([...pattern.matchAll(/\{(\w+)\s*[,}]/g)].map((m) => m[1]))].sort();
}

describe('pickLocale', () => {
	it('takes the first preference the Companion speaks', () => {
		expect(pickLocale(['ja', 'fr', 'en'])).toBe('fr');
		expect(pickLocale(['en-GB', 'fr'])).toBe('en');
		// The three #102 added are spoken now, not skipped over.
		expect(pickLocale(['de', 'fr'])).toBe('de');
		expect(pickLocale(['it-CH', 'en'])).toBe('it');
		expect(pickLocale(['es-MX'])).toBe('es');
	});

	it('matches a regional variant on its language', () => {
		// We ship no fr-CA catalogue, and French still serves that user far
		// better than English does.
		expect(pickLocale(['fr-CA'])).toBe('fr');
		expect(pickLocale(['FR-be'])).toBe('fr');
		expect(pickLocale(['en-US'])).toBe('en');
	});

	it('falls back to English when it recognises nothing', () => {
		expect(pickLocale(['pt', 'ja'])).toBe('en');
		expect(pickLocale([])).toBe('en');
		expect(pickLocale(undefined)).toBe('en');
	});
});

describe('the catalogues', () => {
	it('translate the same set of keys', () => {
		// A key present in English and absent in another language is a screen
		// that silently reads English to that user — the fallback exists for a
		// key added in a hurry, not as a way to ship a partial catalogue.
		for (const locale of LOCALES) {
			expect(Object.keys(catalogues[locale]).sort(), locale).toEqual(Object.keys(en).sort());
		}
	});

	it('interpolate the same placeholders under every key', () => {
		// A translation that drops `{count}` or renames it `{n}` formats fine
		// and shows a sentence with a hole in it. Plural branches are compared
		// by name only: how many forms a language needs is that language's
		// business.
		for (const locale of LOCALES) {
			for (const key of Object.keys(en) as (keyof typeof en)[]) {
				expect(placeholders(catalogues[locale][key] ?? ''), `${locale} ${key}`).toEqual(
					placeholders(en[key])
				);
			}
		}
	});

	it('say whether a native speaker reviewed them', () => {
		// `catalogue.review` is the one entry that is not copy: it is the
		// catalogue's own statement of its provenance, which CONTRIBUTING.md
		// tells a reviewer how to change. English and French were written by
		// the people who own the product; the other three were produced
		// without a native reviewer and must say so until one has.
		expect(en['catalogue.review']).toBe('reviewed');
		expect(fr['catalogue.review']).toBe('reviewed');
		for (const locale of ['it', 'es', 'de'] as const) {
			expect(catalogues[locale]['catalogue.review'], locale).toMatch(/^unreviewed — /);
		}
	});

	it('format every message in every language', () => {
		// The point is the patterns: an unbalanced brace or an unknown ICU
		// function throws at format time, which would be a blank screen.
		const values = {
			count: 2,
			example: 'example.com',
			domain: 'example.com',
			expected: '0.1.0',
			actual: '0.2.0',
			hint: 'k3y9',
			status: 401,
			model: 'qwen',
			scoped: 'OSID',
			running: '1789839442194',
			shipped: '1789900000000',
			path: '/nowhere',
			https: 'https://',
			localhost: 'http://localhost',
			origin: 'http://localhost:4173',
			owner: '@you:example.com',
			id: '@you:example.com',
			errcode: 'M_PASSWORD_TOO_SHORT',
			detail: 'the homeserver said no',
			date: '18/09/2026',
			network: 'WhatsApp',
			// The register's moves in the dashboard feed (#255).
			members: '24',
			threshold: '20',
			device: 'the laptop in the kitchen',
			started: '2026-09-18T07:00:00.000Z',
			seconds: 20,
			rooms: 3,
			sensor: '@sensor:example.com',
			user: '@you:example.com',
			names: '__Secure-1PSID, __Secure-1PSIDTS',
			emoji: '🐢',
			// Screens 4 and 5 (ticket #69). `networks` is a list the screen has
			// already joined, `state` a consent state, `when` a formatted
			// relative time — never a contact, which is the whole point of the
			// dashboard's feed.
			networks: 'WhatsApp, Signal',
			persona: 'assistant',
			state: 'granted',
			when: '2 minutes ago',
			error: 'consent_store_unavailable',
			version: '0.1.0',
			// The deployment's own address, as /recover reports it when nothing
			// answered there (ticket #115).
			url: 'http://twalk.example:8009',
			// The account the Sensor posted a reply as, in its report of what the
			// reply reached (#216).
			postedAs: '@you:example.com',
			// The two homeservers a refused Matrix connection names (#138):
			// the one the user asked for, and the one this deployment drives.
			wanted: 'linagora.com',
			deployment: 'twalk.example',
			// An identity provider as a homeserver advertises it: the sign-in
			// screen labels its button with the provider's own name, because
			// that is what a user recognises (ticket #112).
			provider: 'Connect with Twake',
			// The room chooser's counts (ticket #137): how many rooms the search
			// is showing, out of how many the account has.
			shown: 4,
			total: 112,
			// The approval screen (ticket #100). `sequence` is where a reply
			// landed on the bus, `attempt` which suggestion this is for one
			// message, `type` a contract event type — the identity of the
			// message being answered, which is all this screen is given of it.
			sequence: 4217,
			attempt: 1,
			type: 'fr.linagora.twalk.inbound.message.received.v1',
			// The conversation chooser (ticket #143). `people` is a member
			// count, `conversations` how many rooms one decision covers,
			// `starting`/`stopping` the two directions of that decision, and
			// `observing` how many the Sensor is already inside. `bridge` and
			// `account` are the two halves of #171's diagnosis: which bridge
			// answered and which Matrix account the register asked as.
			people: 246,
			conversations: 3,
			starting: 2,
			stopping: 1,
			observing: 1,
			bridge: 'mautrix-whatsapp',
			account: '@whatsappbot:twalk.localhost',
			reason: 'M_FORBIDDEN',
			name: 'Échecs en Yvelines',
			// The login renderer (ticket #175). `field` is one field's own name
			// as its bridge gave it, and `found` what a step asks for now
			// against the `expected` an explanation was written for — the two
			// halves of a step whose shape has drifted from its copy (ADR 0030).
			field: 'Phone number',
			found: 'password',
			// The disclosure's record (#121): who turned it off, and when. `date`
			// and `reason` are above.
			actor: '@you:example.com'
		};
		for (const locale of LOCALES) {
			for (const key of Object.keys(en) as (keyof typeof en)[]) {
				expect(translate(locale, key, values), `${locale} ${key}`).toBeTruthy();
			}
		}
	});

	it('carry the contract\'s disclosure sentence, word for word (#121)', () => {
		// `settings.disclosure.sentence` is what the settings card shows as the
		// example of what a contact reads, in the interface's language. The
		// sentence has one authority — `contracts/disclosure/v1/sentences.json`,
		// which the SDK and the Companion Gateway read — and the Companion
		// cannot read it at build time (the Gateway image's Node stage copies
		// `companion/` alone), so this is a copy, pinned here the way the SDK
		// pins its own: a sentence changed in the contract fails this test
		// rather than leaving the card showing what a contact no longer reads.
		const contract = JSON.parse(
			readFileSync(
				new URL('../../../../contracts/disclosure/v1/sentences.json', import.meta.url),
				'utf8'
			)
		) as Record<string, string>;
		expect(Object.keys(contract).sort()).toEqual([...LOCALES].sort());
		for (const locale of LOCALES) {
			expect(catalogues[locale]['settings.disclosure.sentence'], locale).toBe(contract[locale]);
		}
	});

	it('pluralise by the rules of their own language', () => {
		expect(translate('en', 'gate.intro', { count: 1 })).toContain('One thing');
		expect(translate('en', 'gate.intro', { count: 3 })).toContain('3 things');
		expect(translate('fr', 'gate.intro', { count: 1 })).toContain('un élément');
		expect(translate('fr', 'gate.intro', { count: 3 })).toContain('3 éléments');
		// One of the three new ones, so the plural rule is exercised there too.
		expect(translate('de', 'dashboard.messages.count', { count: 1 })).toBe('1 Nachricht');
		expect(translate('de', 'dashboard.messages.count', { count: 3 })).toBe('3 Nachrichten');
	});

	it('interpolate named placeholders', () => {
		expect(translate('fr', 'onboarding.domain', { domain: 'maison.fr' })).toContain('maison.fr');
		expect(translate('en', 'error.body', { path: '/nope' })).toContain('/nope');
	});
});
