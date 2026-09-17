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
			origin: 'http://localhost:4173'
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
