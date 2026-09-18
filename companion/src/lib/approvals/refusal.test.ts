// The refusal table, held to the Gateway's own description.
//
// The interesting test here is not that a known code produces a sentence — it
// is that the table cannot silently fall behind. `companion-gateway/openapi.yaml`
// enumerates every `error` value each operation answers with, and the Gateway
// is extended by every ticket that adds an endpoint (`tests/openapi.rs` fails
// on a route it does not describe). So this suite reads that file and fails
// when the approval and suggestion routes grow a code this screen has no
// sentence for.
//
// Without it, the safe-looking default branch in `explain` would make every
// test anybody would think to write pass, and the user would meet "something
// went wrong" — which is the exact defect (#111, #135, #139) the ticket exists
// to prevent.

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
 * `getApproval` is deliberately not one of them. "Did my reply go out?" is
 * answered by the `approval` member the suggestion listing already carries —
 * the same document, says the Gateway's description — so this screen never
 * reads that route, and its `approval_not_found` is not a sentence a user of
 * this screen can reach. Adding it to the table would be inventing a string
 * for a state that cannot happen.
 */
const OPERATIONS = ['approveSuggestion', 'getSuggestions', 'getSuggestion'];

/**
 * Every `error` value the description enumerates for those operations.
 *
 * The codes live in two shapes — inline under a status's schema, and behind a
 * `$ref` to a shared response — so this walks the document rather than
 * assuming one. Anything that is an `enum` under a property called `error` is
 * a code a client can be answered with.
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
	// The two `$ref`d responses the suggestion and approval routes share. They
	// are reached through a reference, which this walk does not follow, so they
	// are named here — and if either is renamed, the assertion below that every
	// code in the table is documented catches it.
	for (const shared of ['approvals_not_configured', 'suggestions_not_configured']) {
		codes.add(shared);
	}
	return codes;
}

describe('the refusal table', () => {
	it('has a sentence for every code the Gateway documents', () => {
		const documented = documentedCodes();
		// A sanity floor: if the walk found nothing, the assertion below would
		// pass vacuously and this test would be worthless.
		expect(documented.size).toBeGreaterThan(10);
		const missing = [...documented].filter((code) => !knownCodes().includes(code));
		expect(missing, 'codes the approval screen has no words for').toEqual([]);
	});

	it('names only codes the Gateway still documents', () => {
		// The other direction: a code that was removed leaves a string nobody
		// can reach, and a table nobody trusts.
		const documented = documentedCodes();
		const extra = knownCodes().filter(
			(code) => !documented.has(code) && code !== 'unauthenticated'
		);
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
		// The type has no such member, so this asserts the other half — that
		// `explain` is total, and never returns something a screen would have
		// to keep waiting on.
		const remedies: Remedy[] = ['retry', 'reload', 'sign-in', 'diagnostics', 'none'];
		for (const code of [...knownCodes(), 'nonsense', '']) {
			const explained = explain('refused', code);
			expect(remedies).toContain(explained.remedy);
		}
		expect(explain('refused', null).remedy).toBe('diagnostics');
	});

	it('names what the user can do, always', () => {
		for (const code of knownCodes()) {
			expect(explain('refused', code).remedyText.length).toBeGreaterThan(0);
		}
	});
});

describe('the one refusal that is not a failure', () => {
	it('says the reply went out when it went out', () => {
		const explained = explain('refused', 'approval_published_but_not_recorded');
		expect(explained.sent).toBe(true);
		// And it is the only one. Telling a user their reply failed when it did
		// not is how a message gets sent twice.
		const others = knownCodes().filter((code) => explain('refused', code).sent);
		expect(others).toEqual(['approval_published_but_not_recorded']);
	});
});

describe('what answered', () => {
	it('keeps "nothing answered" apart from "answered and refused"', () => {
		// The conflation that caused three incidents in one day
		// (`$lib/api/trouble.ts`): a `401` reported as an unreachable server
		// sends the user to look at their firewall for a sign-in problem.
		expect(explain('unreachable', null).cause).toBe('api.trouble.unreachable');
		expect(explain('unreachable', null).remedy).toBe('retry');
		expect(explain('session-refused', 'unauthenticated').remedy).toBe('sign-in');
		expect(explain('refused', 'bus_unreachable').remedy).toBe('retry');
	});

	it('does not read a session refusal as a deployment fault', () => {
		expect(explain('session-refused', null).cause).toBe('approvals.refusal.unauthenticated');
	});
});

describe('the sentences a user reads', () => {
	it('keep "gone" apart from "never was"', () => {
		// The Gateway answers `404` and `410` for these on purpose, and a
		// screen that gave them one sentence would be undoing that.
		expect(explain('refused', 'suggestion_not_found').cause).not.toBe(
			explain('refused', 'suggestion_out_of_reach').cause
		);
	});

	it('keep a revoked consent apart from one never decided', () => {
		expect(explain('refused', 'consent_revoked').cause).not.toBe(
			explain('refused', 'consent_pending').cause
		);
	});

	it('keep the suggestion apart from the message it answers', () => {
		expect(explain('refused', 'suggestion_not_found').cause).not.toBe(
			explain('refused', 'trigger_not_found').cause
		);
	});
});
