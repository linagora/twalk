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
import { scopeFor } from './scope';

export type Network = components['schemas']['Network'];
export type RecordedDecision = components['schemas']['RecordedConsentDecision'];

/** The contract's networks, so a string from a screen can be checked. */
export const NETWORKS: readonly Network[] = [
	'whatsapp',
	'telegram',
	'signal',
	'discord',
	'sms',
	'matrix'
];

export function isNetwork(value: string): value is Network {
	return (NETWORKS as readonly string[]).includes(value);
}

/** Why an activation did not happen, in the codes a screen branches on. */
export type ActivationFailure =
	/** The perimeter was empty: there is nothing to activate the persona on. */
	| { kind: 'no-networks' }
	/** A network value the contract does not have, which is a bug here. */
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
 * `granted` activates it on exactly `networks`; `revoked` pauses it on them.
 * The Gateway stamps `old_state`, `occurred_at` and `actor` itself, and
 * answers `200` rather than `201` when the identical decision was already in
 * the journal — a replay is a success here, because the state the user asked
 * for is the state that holds.
 */
export async function decideOnPersona(options: {
	persona: string;
	state: 'granted' | 'revoked';
	networks: readonly string[];
	reason?: string;
}): Promise<ActivationResult> {
	const scope = scopeFor(options.networks);
	if (scope.length === 0) {
		return { ok: false, failure: { kind: 'no-networks' } };
	}
	const networks: Network[] = [];
	for (const network of scope) {
		if (!isNetwork(network)) {
			return { ok: false, failure: { kind: 'unknown-network', network } };
		}
		networks.push(network);
	}

	let answer;
	try {
		answer = await gateway.POST('/api/consent/decisions', {
			body: {
				subject: { type: 'persona', id: options.persona },
				new_state: options.state,
				scope: { networks },
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

/** Activate a persona on the networks the user chose. */
export function activatePersona(persona: string, networks: readonly string[]) {
	return decideOnPersona({ persona, state: 'granted', networks });
}

/** Pause it: it keeps running, and receives nothing. */
export function pausePersona(persona: string, networks: readonly string[]) {
	return decideOnPersona({ persona, state: 'revoked', networks });
}
