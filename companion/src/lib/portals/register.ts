// Reading the portal register, and writing the user's decision to it.
//
// The seam between the screen and `GET /api/portals` / `POST
// /api/portals/observation`, kept here so the component holds no Gateway
// vocabulary and the reduction is testable.
//
// # Why the bridges come through untouched
//
// The register's answer lists **every configured bridge, readable or not**,
// and since #171 says which account it asked as and how many rooms that
// account is in. That is not diagnostic noise to drop on the way to a list of
// conversations: `absent: 0` with every bridge readable used to mean either
// "your bridges have built no conversations yet" or "the register asked an
// account that is in no rooms", and a deployment with 32 portal rooms was told
// the second and read it as the first. So the screen renders it, and
// [`unreadable`] and [`askedNothing`] are the two shapes it has to be able to
// say.

import { gateway } from '$lib/api/client';
import { troubleOf, type ApiTrouble } from '$lib/api/trouble';
import type { components } from '$lib/api/schema';

import { rows, type ConversationRow, type Portal } from './conversations';
import { batched, requests } from './selection';

export type BridgeReading = components['schemas']['PortalBridgeReading'];
export type Summary = components['schemas']['PortalSummary'];
export type Outcome = components['schemas']['PortalObservationOutcome'];

export interface Register {
	readonly rows: readonly ConversationRow[];
	readonly summary: Summary;
	readonly bridges: readonly BridgeReading[];
	/**
	 * Where a member count becomes a crowd the user must acknowledge, as the
	 * Gateway serves it (#252). The number has one owner and it is not this
	 * screen: the register applies it too when a conversation's room is
	 * replaced (ADR 0029), so a threshold that lived here alone would be one
	 * the two could disagree about.
	 */
	readonly crowdThreshold: number;
}

/** What a failed read was, so the screen can say which (`trouble.ts`). */
export type ReadFailure =
	| { readonly kind: 'trouble'; readonly trouble: ApiTrouble }
	/** `503 portals_not_configured`: this deployment holds no register. */
	| { readonly kind: 'not-configured'; readonly detail: string };

export type ReadResult =
	| { readonly ok: true; readonly register: Register }
	| { readonly ok: false; readonly failure: ReadFailure };

/**
 * The register as the screen needs it.
 *
 * `503 portals_not_configured` is kept apart from every other failure, because
 * it is the one that is neither the network's fault nor the session's: the
 * operator has not set `GATEWAY_BRIDGE_<ID>_AS_TOKEN`, and the Gateway says
 * so in its own words. Reporting that as "the server could not be reached" is
 * the conflation `$lib/api/trouble.ts` exists to prevent, one level up.
 */
export async function readRegister(): Promise<ReadResult> {
	const answer = await gateway.GET('/api/portals').catch(() => null);
	if (answer === null || answer.error !== undefined) {
		if (answer !== null && answer.response.status === 503) {
			const body = answer.error as { detail?: string } | undefined;
			return {
				ok: false,
				failure: { kind: 'not-configured', detail: body?.detail ?? '' }
			};
		}
		return { ok: false, failure: { kind: 'trouble', trouble: troubleOf(answer) } };
	}
	const portals = answer.data.portals as Portal[];
	return {
		ok: true,
		register: {
			rows: rows(portals),
			summary: answer.data.summary,
			bridges: answer.data.bridges,
			crowdThreshold: answer.data.crowd_threshold
		}
	};
}

/** The bridges whose conversations are missing from the list entirely. */
export function unreadable(bridges: readonly BridgeReading[]): BridgeReading[] {
	return bridges.filter((bridge) => !bridge.readable);
}

/**
 * The bridges that answered and whose asking account is in **no rooms at all**.
 *
 * #171's shape, and the reason it is separate from [`unreadable`]: such a
 * bridge is not broken as far as the Gateway can tell, and its zero is either
 * the truth (no conversation has become active yet) or the register asking the
 * appservice's `sender_localpart` instead of the bridge's bot. The screen
 * cannot decide which, and does not pretend to — it states both possibilities
 * and names the account, which is what the deployment could not say at all.
 */
export function askedNothing(bridges: readonly BridgeReading[]): BridgeReading[] {
	return bridges.filter((bridge) => bridge.readable && bridge.joined_rooms === 0);
}

export type ApplyResult =
	| { readonly ok: true; readonly outcomes: readonly Outcome[] }
	| { readonly ok: false; readonly trouble: ApiTrouble; readonly outcomes: readonly Outcome[] };

/**
 * Applies a selection, and reports every outcome it got before it stopped.
 *
 * A failure part-way through keeps what landed: `POST
 * /api/portals/observation` answers one outcome per room, a batch of 256 is a
 * batch of 256 decisions, and throwing them away because a later call failed
 * would tell the user nothing happened when some of it did.
 */
export async function applySelection(
	all: readonly ConversationRow[],
	selected: ReadonlySet<string>
): Promise<ApplyResult> {
	const outcomes: Outcome[] = [];
	for (const call of requests(all, selected)) {
		for (const rooms of batched(call.rooms)) {
			const answer = await gateway
				.POST('/api/portals/observation', { body: { rooms: [...rooms], observed: call.observed } })
				.catch(() => null);
			if (answer === null || answer.error !== undefined) {
				return { ok: false, trouble: troubleOf(answer), outcomes };
			}
			outcomes.push(...answer.data.outcomes);
		}
	}
	return { ok: true, outcomes };
}
