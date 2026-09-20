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

/**
 * The registry as read: the connections when the Gateway answered, or the
 * reason it could not be read. `trouble === null` is "read"; a screen that
 * has not asked yet holds [`NOT_READ_YET`].
 */
export type Registry =
	| { readonly connections: readonly Connection[]; readonly trouble: null }
	| { readonly connections: readonly []; readonly trouble: ApiTrouble };

/** What a screen holds before its first read: nothing, and no trouble either. */
export const NOT_READ_YET: Registry = { connections: [], trouble: null };

/** `GET /api/connections`, never thrown. */
export async function loadRegistry(): Promise<Registry> {
	const answer = await gateway.GET('/api/connections').catch(() => null);
	if (answer === null || answer.error !== undefined || answer.data === undefined) {
		return { connections: [], trouble: troubleOf(answer) };
	}
	return { connections: answer.data.connections, trouble: null };
}

/**
 * The two reads a screen about connections makes, together, under one
 * policy: the registry says which connections there are, `GET /api/bridges`
 * what each one's transport reports. Either failing is the same kind of
 * fact — the deployment could not be asked — and `trouble` names the first
 * one that failed, so no screen composes a policy of its own (#272).
 */
export async function loadRegistryAndBridges(): Promise<
	| { readonly registry: readonly Connection[]; readonly bridges: readonly ConfiguredBridge[]; readonly trouble: null }
	| { readonly registry: readonly []; readonly bridges: readonly []; readonly trouble: ApiTrouble }
> {
	const [registry, listed] = await Promise.all([
		loadRegistry(),
		gateway.GET('/api/bridges').catch(() => null)
	]);
	if (registry.trouble !== null) {
		return { registry: [], bridges: [], trouble: registry.trouble };
	}
	if (listed === null || listed.error !== undefined || listed.data === undefined) {
		return { registry: [], bridges: [], trouble: troubleOf(listed) };
	}
	return { registry: registry.connections, bridges: listed.data.bridges, trouble: null };
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

/**
 * Whether a kind has several connections and the screen was told none:
 * the case `pick` answers `null` to that is not "nothing configured" but
 * "which one?", and the screen says so.
 */
export function isAmbiguous(
	connections: readonly Connection[],
	kind: string,
	named: string | null
): boolean {
	return named === null && ofKind(connections, kind).length > 1;
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

/**
 * A link to one of a kind's screens: the route, and `?connection=` when the
 * kind has several — so the reference deployment's links read as they always
 * did. `extra` are further query members (`relink=…`), appended after the
 * connection with the right separator: a caller that did `${route}?relink`
 * on a route already carrying a query produced `?connection=x?relink=…`,
 * which no screen could read.
 */
export function linkTo(
	route: string,
	connection: Connection | null,
	siblings: readonly Connection[],
	extra: Record<string, string> = {}
): string {
	const query = new URLSearchParams();
	if (connection !== null && siblings.length > 1) {
		query.set('connection', connection.id);
	}
	for (const [key, value] of Object.entries(extra)) {
		query.set(key, value);
	}
	const text = query.toString();
	return text === '' ? route : `${route}?${text}`;
}

/** The connection a URL names, or `null`. */
export function connectionNamedBy(url: URL): string | null {
	const named = url.searchParams.get('connection');
	return named === null || named === '' ? null : named;
}
