// What a persona is activated *on*: the networks the user actually connected.
//
// ADR 0013 is categorical — "activation does not spread: connecting a new
// network leaves every persona inactive on it until the user says otherwise" —
// so the scope of an activation is decided once, from what is connected at the
// moment the user presses the button, and is never widened afterwards. A
// network connected next week is simply not in any decision the user took.
//
// # Where "connected" comes from, and the one network it cannot come from
//
// From `$lib/networks/connection.ts`, which is the single place that judgement
// is made, and from nowhere else.
//
// This module used to make it itself, as `bridge.login?.state !== 'complete'`
// — the **login process**, which lives in the Gateway's memory for at most
// thirty minutes and is gone after a restart or after the login it describes
// finished. That is the defect #108 named, and it survived here because #108
// was scoped by directory while the defect is defined by source of truth
// (#142). The consequence was worse on this screen than on the picker: the
// list came back empty on a deployment with WhatsApp and Signal both
// connected, so there was nothing to tick and **no persona could be activated
// at all**.
//
// A network is in the perimeter when `isConnected` says so, which is the
// contract's `connected` and nothing rounded up to it: a bridge that is
// starting or reconnecting has no link to scope a consent decision onto yet,
// and the screen would be claiming one on the user's behalf.
//
// Matrix is the exception, and the reason is in the Gateway's own description:
// the existing-account path (screen 3d) invites the Sensor into the rooms the
// user ticked, using the user's own access token, and **forgets the token when
// the call returns** (ADR 0011). Nothing on either side records that it
// happened — there is no `GET /api/bootstrap/rooms`. So the Companion cannot
// prove the Matrix network is connected, and it does not pretend to: the row
// is offered unticked, with the reason written on the screen. Guessing would
// scope a consent decision to a network on the user's behalf, which is the
// precise behaviour this project exists to prevent.

import { connectionOf, isConnected, type ConfiguredBridge } from '$lib/networks/connection';

/**
 * One row of `GET /api/bridges`, whole.
 *
 * Whole, and not a hand-written subset: the two members that matter have
 * confusable names — `connection` is the bridge's own answer about the link it
 * holds, `login` is a login process in the Gateway's memory — and a subset
 * that kept only the second is how this module came to read the wrong one
 * (#142).
 */
export type BridgeRow = ConfiguredBridge;

/** A network offered as part of an activation's perimeter. */
export interface ScopeOption {
	readonly network: string;
	/**
	 * Whether the Gateway can see that this network is connected. `false` for
	 * `matrix`, which nothing records — see the module note.
	 */
	readonly proven: boolean;
	/** Whether the row starts ticked. Only a proven connection does. */
	readonly preselected: boolean;
}

/** The `matrix` network, whose connection leaves no trace to read back. */
export const MATRIX_NETWORK = 'matrix';

/**
 * The perimeter screen 4 offers: every network a bridge says is connected,
 * ticked, then Matrix, unticked.
 *
 * Bridge order is the Gateway's — configuration order — so two deployments
 * with the same bridges draw the same screen.
 */
export function scopeOptions(bridges: readonly ConfiguredBridge[]): ScopeOption[] {
	const options: ScopeOption[] = [];
	for (const bridge of bridges) {
		if (!isConnected(connectionOf(bridge))) {
			continue;
		}
		if (options.some((option) => option.network === bridge.network)) {
			continue;
		}
		options.push({ network: bridge.network, proven: true, preselected: true });
	}
	if (!options.some((option) => option.network === MATRIX_NETWORK)) {
		options.push({ network: MATRIX_NETWORK, proven: false, preselected: false });
	}
	return options;
}

/** The networks a freshly drawn screen 4 would submit. */
export function defaultSelection(options: readonly ScopeOption[]): string[] {
	return options.filter((option) => option.preselected).map((option) => option.network);
}

/**
 * The scope of the decision, in the order the Gateway will sort it into
 * anyway. Sorted here too so that two spellings of one perimeter are one
 * request, and so the test's expectation is stable.
 */
export function scopeFor(selected: readonly string[]): string[] {
	return [...new Set(selected)].sort();
}
