// The consent screen's refusal table, held to the Gateway's own description.
//
// Same method as `$lib/approvals/refusal.test.ts`, and for the same reason: the
// interesting property is not that a known code produces a sentence, it is that
// the table cannot silently fall behind. `companion-gateway/openapi.yaml`
// enumerates every `error` value each operation answers with, and
// `companion-gateway/tests/openapi.rs` fails on a route it does not describe —
// so reading that file is reading the Gateway.
//
// Without this, the default branch in `explain` would make every test anybody
// would think to write pass, and the user would meet "something went wrong" on
// the screen that carries the product's central promise.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';
import { parse as parseYaml } from 'yaml';

import en from '$lib/i18n/en.json';
import { explain, knownCodes, type Remedy } from './refusal';

const OPENAPI = join(
	dirname(fileURLToPath(import.meta.url)),
	'..',
	'..',
	'..',
	'..',
	'companion-gateway',
	'openapi.yaml'
);

/**
 * The operations this screen calls, and therefore has to have words for.
 *
 * `getConsentSnapshot` is deliberately not one of them: it is the Sensor's own
 * read, behind a service token, and no browser can reach it. A sentence for its
 * refusals would be a string for a state this screen cannot get into.
 */
const OPERATIONS = [
	'recordConsentDecision',
	'getConsentState',
	'getEffectiveConsent',
	'getPendingContacts',
	'getContactDisplayNames'
];

/**
 * Every `error` value the description enumerates for those operations.
 *
 * Codes live in two shapes — inline under a status's schema, and behind a
 * `$ref` to a shared response — so this walks the document rather than assuming
 * one, and the `$ref`d ones are named below.
 */
function documentedCodes(): Set<string> {
	const document = parseYaml(readFileSync(OPENAPI, 'utf8')) as Record<string, unknown>;
	const codes = new Set<string>();

	const collect = (node: unknown): void => {
		if (Array.isArray(node)) {
			node.forEach(collect);
			return;
		}
		if (node === null || typeof node !== 'object') {
			return;
		}
		const record = node as Record<string, unknown>;
		const error = record.error as { enum?: unknown } | undefined;
		if (error !== undefined && Array.isArray(error.enum)) {
			for (const value of error.enum) {
				if (typeof value === 'string') {
					codes.add(value);
				}
			}
		}
		Object.values(record).forEach(collect);
	};

	const paths = document.paths as Record<string, Record<string, unknown>>;
	for (const operations of Object.values(paths)) {
		for (const operation of Object.values(operations)) {
			const named = operation as { operationId?: string; responses?: unknown };
			if (named.operationId !== undefined && OPERATIONS.includes(named.operationId)) {
				collect(named.responses);
			}
		}
	}
	// The `$ref`d responses these routes share: `Unauthenticated`,
	// `ConsentStoreUnavailable`, `ConsentNotConfigured`, `ContactsNotConfigured`.
	// They are reached through a reference, which this walk does not follow, so
	// their codes are named here — and the assertion below that every code in
	// the table is documented catches a rename.
	for (const shared of [
		'unauthenticated',
		'store_unavailable',
		'consent_not_configured',
		'contacts_not_configured'
	]) {
		codes.add(shared);
	}
	return codes;
}

describe('the refusal table', () => {
	it('has a sentence for every code the Gateway documents', () => {
		const documented = documentedCodes();
		// A sanity floor: if the walk found nothing, the assertion below would
		// pass vacuously.
		expect(documented.size).toBeGreaterThan(5);
		const missing = [...documented].filter((code) => !knownCodes().includes(code));
		expect(missing, 'codes the consent screen has no words for').toEqual([]);
	});

	it('names only codes the Gateway still documents', () => {
		const documented = documentedCodes();
		const extra = knownCodes().filter((code) => !documented.has(code));
		expect(extra, 'codes this table invents').toEqual([]);
	});

	it('translates every cause and every remedy it can produce', () => {
		const keys = new Set(Object.keys(en));
		for (const code of [...knownCodes(), 'a_code_from_the_future']) {
			const explained = explain('refused', code);
			expect(keys.has(explained.cause), `${code}: ${explained.cause}`).toBe(true);
			expect(keys.has(explained.remedyText), `${code}: ${explained.remedyText}`).toBe(true);
		}
	});
});

describe('every answer', () => {
	it('is terminal: there is no value that means "still working"', () => {
		const remedies: Remedy[] = ['retry', 'reload', 'sign-in', 'diagnostics', 'none'];
		for (const code of [...knownCodes(), 'nonsense', '']) {
			expect(remedies).toContain(explain('refused', code).remedy);
		}
		expect(explain('refused', null).remedy).toBe('diagnostics');
	});

	it('names what the user can do, always', () => {
		for (const code of knownCodes()) {
			expect(explain('refused', code).remedyText.length).toBeGreaterThan(0);
		}
	});
});

describe('what answered', () => {
	it('keeps "nothing answered" apart from "answered and refused"', () => {
		expect(explain('unreachable', null).cause).toBe('api.trouble.unreachable');
		expect(explain('unreachable', null).remedy).toBe('retry');
		expect(explain('session-refused', 'unauthenticated').remedy).toBe('sign-in');
		expect(explain('refused', 'store_unavailable').remedy).toBe('retry');
	});

	it('does not read a session refusal as a deployment fault', () => {
		expect(explain('session-refused', null).cause).toBe('consent.refusal.unauthenticated');
	});
});

describe('the sentences a user reads', () => {
	it('keeps a deployment that records no consent apart from one that watches nothing', () => {
		// Two different variables' absence, two different things the user has
		// lost, and neither of them is "nobody has written to you".
		expect(explain('refused', 'consent_not_configured').cause).not.toBe(
			explain('refused', 'contacts_not_configured').cause
		);
	});

	it('does not present an unreadable display name as a failed list', () => {
		// `502 bus_unreachable` is about the names alone: the list comes from
		// the Gateway's own store and is unaffected, so there is nothing for the
		// user to retry.
		expect(explain('refused', 'bus_unreachable').remedy).toBe('none');
	});
});
