// The contract's networks, as runtime values.
//
// The generated client knows them as a type (`components['schemas']['Network']`),
// and a type is gone at runtime — so a screen that has to check a string it was
// handed needs a list. This is the one list: two of them had grown, one per
// screen, and both had quietly fallen behind the contract when `email` joined it
// (#268). The authority is `contracts/cloudevents/v1/definitions/network.schema.json`,
// and `contract.test.ts` reads that file and compares — a network added to the
// contract fails here until this list knows it, and the consent screen no longer
// drops a row about it on the floor.

import type { components } from '$lib/api/schema';

export type Network = components['schemas']['Network'];

/** Every network the contract names, in the contract's order. */
export const NETWORKS: readonly Network[] = [
	'whatsapp',
	'telegram',
	'signal',
	'discord',
	'sms',
	'matrix',
	'email'
];

export function isNetwork(value: string): value is Network {
	return (NETWORKS as readonly string[]).includes(value);
}
