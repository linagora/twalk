// Activating and pausing a persona: one call, one consent decision.
//
// ADR 0013 again, and it is worth stating in the code that makes the request
// because the shape of this module is the decision: there is no persona
// control API to call, so activation is `POST /api/consent/decisions` with a
// `persona` subject and nothing else. Pausing is the same call with
// `new_state: revoked`.
//
// **Paused means starved, not stopped.** A revoked persona still runs and is
// still supervised by Hermes; what changes is that no event reaches it. The
// copy on both screens says so (`persona.pause.*`), because a user who reads
// "paused" as "the process is gone" will later be surprised to find it
// running.

import { gateway } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { isNetwork, type Network } from '$lib/networks/contract';
import { scopeFor } from './scope';

// The contract's networks live in `$lib/networks/contract` — one runtime list,
// tested against the contract — so a string from a screen is checked against
// what the Gateway will accept and not against a copy kept here (#268).
export { isNetwork, NETWORKS, type Network } from '$lib/networks/contract';
export type RecordedDecision = components['schemas']['RecordedConsentDecision'];

/**
 * The shape of a connection id, as the contract spells it
 * (`contracts/cloudevents/v1/definitions/connection.schema.json`). A copy,
 * held to the contract by `activation.test.ts`, so the two cannot drift.
 */
export const CONNECTION_ID_PATTERN = '^[a-z0-9][a-z0-9-]{0,63}$';
const CONNECTION_ID = new RegExp(CONNECTION_ID_PATTERN);

export function isConnectionId(value: string): boolean {
	return CONNECTION_ID.test(value);
}

/** Why an activation did not happen, in the codes a screen branches on. */
export type ActivationFailure =
	/** The perimeter was empty: there is nothing to activate the persona on. Named `no-networks` since before #272; the perimeter is connections. */
	| { kind: 'no-networks' }
	/** A connection id the contract's shape does not admit, which is a bug here. `network` carries the id; the code is the screen's, kept stable. */
	| { kind: 'unknown-network'; network: string }
	/** The Gateway refused, with its own stable `error` code. */
	| { kind: 'refused'; error: string; status: number }
	/** The Gateway could not be reached at all. */
	| { kind: 'unreachable' };

export type ActivationResult =
	| { ok: true; decision: RecordedDecision }
	| { ok: false; failure: ActivationFailure };

/**
 * Records one decision about a persona.
 *
 * `granted` activates it on exactly `connections`; `revoked` pauses it on
 * them. The scope is connection ids (#270, #272): a persona active on the
 * work WhatsApp is not active on the home one.
 * The Gateway stamps `old_state`, `occurred_at` and `actor` itself, and
 * answers `200` rather than `201` when the identical decision was already in
 * the journal — a replay is a success here, because the state the user asked
 * for is the state that holds.
 */
export async function decideOnPersona(options: {
	persona: string;
	state: 'granted' | 'revoked';
	connections: readonly string[];
	reason?: string;
}): Promise<ActivationResult> {
	const connections = scopeFor(options.connections);
	if (connections.length === 0) {
		return { ok: false, failure: { kind: 'no-networks' } };
	}
	for (const connection of connections) {
		if (!isConnectionId(connection)) {
			return { ok: false, failure: { kind: 'unknown-network', network: connection } };
		}
	}

	let answer;
	try {
		answer = await gateway.POST('/api/consent/decisions', {
			body: {
				subject: { type: 'persona', id: options.persona },
				new_state: options.state,
				scope: { connections },
				...(options.reason === undefined ? {} : { reason: options.reason })
			}
		});
	} catch {
		return { ok: false, failure: { kind: 'unreachable' } };
	}

	if (answer.data !== undefined) {
		return { ok: true, decision: answer.data };
	}
	// Branch on `error`, never on `detail` — the description guarantees the
	// first is stable and says the second is an operator's sentence.
	const error = (answer.error as { error?: string } | undefined)?.error;
	return {
		ok: false,
		failure: {
			kind: 'refused',
			error: error ?? 'unknown',
			status: answer.response?.status ?? 0
		}
	};
}

/** Activate a persona on the connections the user chose. */
export function activatePersona(persona: string, connections: readonly string[]) {
	return decideOnPersona({ persona, state: 'granted', connections });
}

/** Pause it: it keeps running, and receives nothing. */
export function pausePersona(persona: string, connections: readonly string[]) {
	return decideOnPersona({ persona, state: 'revoked', connections });
}
