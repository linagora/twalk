// The registry of connections, and the three questions every screen asks it
// (ADR 0033, #272).
//
// A **card is a connection**, and a **bridge is a connection's transport**.
// Until #272 the Companion joined the catalogue to the deployment with
// `bridges.find((row) => row.network === card.network)` — first match wins —
// which is the defect ADR 0033 named: a deployment with a personal WhatsApp
// and a work one had one card, one login screen and one set of decisions for
// both, and granting a contact on one granted them on the other. So no screen
// resolves a bridge or a decision by network any more: it asks the Gateway's
// registry (`GET /api/connections`) which connections there are, picks the
// one it is about, and reads that connection's `bridge_id` to find its
// transport.
//
// Three rules, each a function below:
//
//   - **Pick, never guess.** A screen about one connection is told which one
//     by the URL (`?connection=`); with no name it takes the kind's only
//     connection, and with two and no name it takes *nothing* and says so.
//   - **A bridge is found by id.** `bridgeOf` reads the connection's
//     `bridge_id` and nothing else.
//   - **A label appears when it disambiguates.** One WhatsApp is "WhatsApp",
//     as it always was; two are "WhatsApp — Home" and "WhatsApp — Work".
//
// Where a deployment has one connection per kind — the reference shape —
// every screen reads exactly as before.

import { gateway } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { troubleOf, type ApiTrouble } from '$lib/api/trouble';
import type { ConfiguredBridge } from '$lib/networks/connection';

export type Connection = components['schemas']['Connection'];

/** The registry as read, or the reason it could not be. */
export interface Registry {
	readonly connections: readonly Connection[];
	/** Whether the Gateway answered. `false` leaves every screen honest about not knowing. */
	readonly known: boolean;
	readonly trouble: ApiTrouble | null;
}

export const UNKNOWN_REGISTRY: Registry = { connections: [], known: false, trouble: null };

/** `GET /api/connections`, never thrown. */
export async function loadRegistry(): Promise<Registry> {
	const answer = await gateway.GET('/api/connections').catch(() => null);
	if (answer === null || answer.error !== undefined || answer.data === undefined) {
		return { connections: [], known: false, trouble: troubleOf(answer) };
	}
	return { connections: answer.data.connections, known: true, trouble: null };
}

/** The connections of one kind, in the registry's order. */
export function ofKind(connections: readonly Connection[], kind: string): Connection[] {
	return connections.filter((connection) => connection.kind === kind);
}

/**
 * The connection a screen is about: the one `named` (which must be of this
 * kind), else the kind's only one, else `null`. Two of the kind and no name
 * is `null` on purpose — the first match winning is the defect this module
 * exists to remove.
 */
export function pick(
	connections: readonly Connection[],
	kind: string,
	named: string | null
): Connection | null {
	const candidates = ofKind(connections, kind);
	if (named !== null) {
		return candidates.find((connection) => connection.id === named) ?? null;
	}
	return candidates.length === 1 ? candidates[0]! : null;
}

/** The bridge carrying a connection, by the id the connection names — never by network. */
export function bridgeOf(
	connection: Connection,
	bridges: readonly ConfiguredBridge[]
): ConfiguredBridge | null {
	if (connection.bridge_id === undefined) {
		return null;
	}
	return bridges.find((bridge) => bridge.bridge_id === connection.bridge_id) ?? null;
}

/** The connection's label when its kind has several, `null` when it is the only one. */
export function labelFor(connection: Connection, siblings: readonly Connection[]): string | null {
	return siblings.length > 1 ? connection.label : null;
}

/** The query string a link to one connection's screen carries. */
export function connectionQuery(connection: Connection, siblings: readonly Connection[]): string {
	return siblings.length > 1 ? `?connection=${encodeURIComponent(connection.id)}` : '';
}

/** The connection a URL names, or `null`. */
export function namedIn(url: URL): string | null {
	const named = url.searchParams.get('connection');
	return named === null || named === '' ? null : named;
}
