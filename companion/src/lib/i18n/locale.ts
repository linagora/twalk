// Which language the Companion speaks, decided from what the browser says it
// wants. Pure: no `navigator` here, so this module loads under prerendering
// and is unit-testable. The caller passes `navigator.languages` (or
// `[navigator.language]`); see `./index.ts`.

/**
 * The locales v0.1 ships (ADR 0016, #102). French first: it is the owner's
 * language. English and French are reviewed; Italian, Spanish and German were
 * produced without a native reviewer, which each catalogue states in its
 * `catalogue.review` entry and `CONTRIBUTING.md` says how to fix.
 */
export const LOCALES = ['fr', 'en', 'it', 'es', 'de'] as const;

export type Locale = (typeof LOCALES)[number];

/**
 * Each language named in itself, for a picker: a German speaker looking for
 * their language finds "Deutsch", not "German" in whatever language the
 * screen happens to be in.
 */
export const LOCALE_NAMES: Record<Locale, string> = {
	fr: 'Français',
	en: 'English',
	it: 'Italiano',
	es: 'Español',
	de: 'Deutsch'
};

/**
 * English is the fallback, not the default: `pickLocale` only reaches it when
 * the browser asked for nothing we speak. A key missing from a catalogue also
 * falls back to it.
 */
export const FALLBACK_LOCALE: Locale = 'en';

function isLocale(value: string): value is Locale {
	return (LOCALES as readonly string[]).includes(value);
}

/**
 * The first of the browser's preferences the Companion speaks.
 *
 * Matches on the language subtag, so `fr-CA`, `fr-BE` and `FR` all mean
 * French: a regional variant we do not have a catalogue for is still better
 * served by its language than by English.
 */
export function pickLocale(preferred: readonly string[] | undefined): Locale {
	for (const tag of preferred ?? []) {
		const language = tag.toLowerCase().split('-')[0];
		if (language !== undefined && isLocale(language)) {
			return language;
		}
	}
	return FALLBACK_LOCALE;
}
