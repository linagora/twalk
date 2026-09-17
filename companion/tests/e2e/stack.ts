// Reading what `tests/real-stack.mjs` brought up, for the specs that need a
// real Gateway and a real Synapse.
//
// Those specs skip themselves when the stack is not there, so `npm test` stays
// a Node-only suite that needs neither Docker nor a Rust toolchain, and
// `npm run test:e2e:stack` is the one that proves the journey. A spec that
// silently passed without the stack would be worse than one that says it did
// not run.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export interface RealStack {
	/** The localpart the Gateway was configured with for this run. */
	owner: string;
	/** `@owner:test.twalk`. */
	ownerId: string;
	serverName: string;
	/** What the user types on screen 1: loopback with Synapse's port. */
	domain: string;
	synapseUrl: string;
	gatewayOrigin: string;
}

const STACK_FILE = join(dirname(fileURLToPath(import.meta.url)), '..', '.real-stack.json');

/** The stack this run created, or `null` when there is none. */
export function realStack(): RealStack | null {
	if (process.env.TWALK_TEST_REAL_STACK !== '1') {
		return null;
	}
	try {
		return JSON.parse(readFileSync(STACK_FILE, 'utf8')) as RealStack;
	} catch {
		return null;
	}
}

/** The reason a spec skipped, for the report to carry. */
export const NO_STACK =
	'needs a real Gateway and Synapse: run `npm run test:e2e:stack` (Docker and cargo required)';
