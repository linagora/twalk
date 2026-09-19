// The two things about i18n that are logic rather than copy: which locale the
// browser's preferences resolve to, and whether the ICU patterns in both
// catalogues actually format.

import { describe, expect, it } from 'vitest';

import en from './en.json';
import fr from './fr.json';
import { pickLocale } from './locale';
import { translate } from './index';

describe('pickLocale', () => {
	it('takes the first preference the Companion speaks', () => {
		expect(pickLocale(['de', 'fr', 'en'])).toBe('fr');
		expect(pickLocale(['en-GB', 'fr'])).toBe('en');
	});

	it('matches a regional variant on its language', () => {
		// We ship no fr-CA catalogue, and French still serves that user far
		// better than English does.
		expect(pickLocale(['fr-CA'])).toBe('fr');
		expect(pickLocale(['FR-be'])).toBe('fr');
		expect(pickLocale(['en-US'])).toBe('en');
	});

	it('falls back to English when it recognises nothing', () => {
		expect(pickLocale(['de', 'ja'])).toBe('en');
		expect(pickLocale([])).toBe('en');
		expect(pickLocale(undefined)).toBe('en');
	});
});

describe('the catalogues', () => {
	it('translate the same set of keys', () => {
		// A key present in one language and absent in the other is a screen
		// that silently reads English to a French user.
		expect(Object.keys(fr).sort()).toEqual(Object.keys(en).sort());
	});

	it('format every message in both languages', () => {
		// The point is the patterns: an unbalanced brace or an unknown ICU
		// function throws at format time, which would be a blank screen.
		const values = {
			count: 2,
			example: 'example.com',
			domain: 'example.com',
			expected: '0.1.0',
			actual: '0.2.0',
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
			found: 'password'
		};
		for (const key of Object.keys(en) as (keyof typeof en)[]) {
			expect(translate('en', key, values), key).toBeTruthy();
			expect(translate('fr', key, values), key).toBeTruthy();
		}
	});

	it('pluralise by the rules of their own language', () => {
		expect(translate('en', 'gate.intro', { count: 1 })).toContain('One thing');
		expect(translate('en', 'gate.intro', { count: 3 })).toContain('3 things');
		expect(translate('fr', 'gate.intro', { count: 1 })).toContain('un élément');
		expect(translate('fr', 'gate.intro', { count: 3 })).toContain('3 éléments');
	});

	it('interpolate named placeholders', () => {
		expect(translate('fr', 'onboarding.domain', { domain: 'maison.fr' })).toContain('maison.fr');
		expect(translate('en', 'error.body', { path: '/nope' })).toContain('/nope');
	});
});
