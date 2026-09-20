// The contract is the one authority for the networks (ADR 0033, #268), and
// `contract.ts` is the Companion's runtime copy of it. Tested against what it
// copies, order included: this is the test the two hand-kept lists it replaced
// never had.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { isNetwork, NETWORKS } from './contract';

const NETWORK_DEFINITION = join(
	dirname(fileURLToPath(import.meta.url)),
	'..',
	'..',
	'..',
	'..',
	'contracts',
	'cloudevents',
	'v1',
	'definitions',
	'network.schema.json'
);

describe('the runtime list of networks', () => {
	const authority = (JSON.parse(readFileSync(NETWORK_DEFINITION, 'utf8')) as { enum: string[] })
		.enum;

	it('is the contract’s definition, in its order', () => {
		expect([...NETWORKS]).toEqual(authority);
	});

	it('recognises every network the contract names and nothing else', () => {
		for (const network of authority) {
			expect(isNetwork(network), network).toBe(true);
		}
		// A bridge id is never a network (ADR 0005), and neither is a kind
		// that is not a network (`calendar`, ADR 0033).
		for (const value of ['gmessages', 'calendar', '', 'WhatsApp']) {
			expect(isNetwork(value), value).toBe(false);
		}
	});
});
