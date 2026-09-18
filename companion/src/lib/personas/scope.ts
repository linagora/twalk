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
// `GET /api/bridges` reports one row per configured bridge with the login it
// holds; a login in state `complete` is a network the user connected, and the
// Gateway can say so without contacting anything.
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

/** One row of `GET /api/bridges`, reduced to what a scope needs from it. */
export interface BridgeRow {
	readonly bridge_id: string;
	readonly network: string;
	readonly login: { readonly state: string } | null;
}

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
export function scopeOptions(bridges: readonly BridgeRow[]): ScopeOption[] {
	const options: ScopeOption[] = [];
	for (const bridge of bridges) {
		if (bridge.login?.state !== 'complete') {
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
