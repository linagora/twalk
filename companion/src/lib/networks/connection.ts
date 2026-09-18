// What a network's card and its management screen know about the link that
// already exists — read from the bridge, never from a login in progress.
//
// # The distinction this file exists to hold
//
// There are two things with confusable names, and the Companion once read the
// wrong one:
//
//   - a **login** (here: a `LinkedAccount`) is the persistent link between the
//     bridge and the user's account on the network. The network's credentials
//     sit behind it, it lives in the bridge, and nothing this browser or the
//     Gateway does can lose it.
//   - a **login process** (`login-view.ts`) is a QR scan somebody is in the
//     middle of. It lives in the Gateway's memory for at most thirty minutes.
//
// `catalogue.ts` used to compute a network's connected badge as
// `bridge?.login?.state === 'complete'` — the second one. So starting a login
// cleared the badge, cancelling one cleared it, and restarting the Gateway
// cleared it, all while the bridge held a live WhatsApp session throughout
// (#108). A user saw that and was about to re-pair a working connection.
//
// So: **nothing in this file may read `ConfiguredBridge.login`.** The
// connected state is `connection`, and `connection` is the bridge's answer.
//
// # The vocabulary is not this file's to invent
//
// The five states are the contract's, defined once by the Gateway's mapping
// table (ticket #56) and carried by `bridge.status.changed.v1`. This module
// re-exports them and adds exactly one value of its own, `unknown`, which is
// not a state of the bridge at all — it is the state of our knowledge when the
// Gateway could not reach it. Reporting `disconnected` there would be the same
// lie in a new place.

import type { components } from '$lib/api/schema';

export type ConfiguredBridge = components['schemas']['ConfiguredBridge'];
export type BridgeConnection = components['schemas']['BridgeConnection'];
export type LinkedAccount = components['schemas']['BridgeLinkedLogin'];

/**
 * The contract's five states, plus `unknown`.
 *
 * `unknown` is deliberately not one of the five: the Gateway answers
 * `state: null` when it could not ask the bridge, and a screen has to be able
 * to say "I cannot tell" rather than guess in either direction.
 */
export type ConnectionState =
	| 'connected'
	| 'starting'
	| 'degraded'
	| 'session_expired'
	| 'disconnected'
	| 'unknown';

/** One network's link, as every screen in this journey reads it. */
export interface NetworkConnection {
	readonly state: ConnectionState;
	/**
	 * Whether the bridge holds a login at all.
	 *
	 * This, and not `state === 'connected'`, is what decides between *Connect*
	 * and *Manage*: a session whose credentials expired is still a link the
	 * user has, and the thing they need is the management screen — to
	 * disconnect it or to re-link it — not a fresh QR flow.
	 */
	readonly linked: boolean;
	/** The account the bridge says is linked, or `null` when none is. */
	readonly account: LinkedAccount | null;
	/** The bridge's own message, for the management screen's detail line. */
	readonly reason: string | null;
}

export const UNKNOWN_CONNECTION: NetworkConnection = {
	state: 'unknown',
	linked: false,
	account: null,
	reason: null
};

/**
 * One `GET /api/bridges` row, read as the link it describes.
 *
 * `null` for a network this deployment configured no bridge for — Matrix, or
 * a bridge the operator has not enabled — which is a different thing from a
 * bridge that answered "no login".
 */
export function connectionOf(bridge: ConfiguredBridge | null | undefined): NetworkConnection {
	if (bridge === null || bridge === undefined) {
		return UNKNOWN_CONNECTION;
	}
	const connection = bridge.connection;
	if (!connection.reachable || connection.state === null) {
		return UNKNOWN_CONNECTION;
	}
	// v0.1 is one login per bridge instance (spec #47), so the first login is
	// the login. A second one would be a deployment that has grown past what
	// this version models, and showing the first is better than showing none.
	const account = connection.logins[0] ?? null;
	return {
		state: connection.state,
		linked: account !== null,
		account,
		reason: account?.reason ?? connection.reason
	};
}

/**
 * Whether this network should be shown as connected — the green check of
 * screen 3.
 *
 * Only `connected`. A `starting` bridge is coming up and a `degraded` one is
 * reconnecting on its own; both are honestly reported as themselves rather
 * than rounded to a tick the user would trust.
 */
export function isConnected(connection: NetworkConnection): boolean {
	return connection.state === 'connected';
}

/**
 * Whether the state is one the user has to act on.
 *
 * Exactly one is: `session_expired`. That is what a session revoked from the
 * user's own phone reports (`BAD_CREDENTIALS` — never `LOGGED_OUT`, which no
 * mautrix bridge emits), and the only way out of it is to re-link. `degraded`
 * is not: the bridge expects to come back by itself and asking the user to do
 * something would be asking for nothing.
 */
export function needsAttention(connection: NetworkConnection): boolean {
	return connection.state === 'session_expired';
}
