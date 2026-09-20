// The shape of a connection id lives in the contract (ADR 0033); the copy
// this module keeps, so a screen can refuse a malformed id before asking the
// Gateway, is held to it here.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { CONNECTION_ID_PATTERN, isConnectionId } from './activation';

const CONNECTION_DEFINITION = join(
	dirname(fileURLToPath(import.meta.url)),
	'..',
	'..',
	'..',
	'..',
	'contracts',
	'cloudevents',
	'v1',
	'definitions',
	'connection.schema.json'
);

describe('the contract is the authority for the shape of a connection id', () => {
	const authority = (JSON.parse(readFileSync(CONNECTION_DEFINITION, 'utf8')) as { pattern: string })
		.pattern;

	it('is the pattern this module copies', () => {
		expect(CONNECTION_ID_PATTERN).toBe(authority);
	});

	it('admits the ids a registry names and refuses the rest', () => {
		for (const id of ['whatsapp', 'wa-work', 'mail-linagora', 'a', '0']) {
			expect(isConnectionId(id), id).toBe(true);
		}
		for (const id of ['', '-a', 'Work', 'a_b', 'a b', 'a'.repeat(65)]) {
			expect(isConnectionId(id), id).toBe(false);
		}
	});
});
