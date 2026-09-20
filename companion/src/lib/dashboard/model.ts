// Screen 5's view model: the Gateway's documents in, the dashboard's rows out.
//
// Every function here is pure, because the interesting half of this screen is
// a judgement — is the system healthy, what does the user do next, and *what
// may be shown at all* — and a judgement rendered inside a component is a
// judgement nothing tests.
//
// # The activity feed is operational only
//
// The wireframe's screen 5 fed "the last 10 events on the bus (received
// messages, agent suggestions, bridge status changes, consent changes)", each
// row naming who wrote. The design review struck that (#74, #69): a home
// screen unlocked in public, in a product whose argument is sovereignty,
// must not be the list of who writes to the user. So the feed carries
// operational events — bridge state, consent decisions, persona activity — and
// a message **count**, and nothing that identifies a correspondent.
//
// That is a rule about *data*, not about copy, and it is enforced here rather
// than in the markup: [`activityFeed`] never puts a contact's id in a row's
// values, and `model.test.ts` asserts that a consent decision on a contact
// renders without it. The Gateway helps — it stores no contact list (#54) and
// the Companion caches none (#66) — but a consent decision does carry the
// contact's Matrix ID, and that is the one place this screen could have leaked
// it.
//
// # What this screen cannot know yet, and says so
//
// Three facts the wireframe assumes have no source in the deployment as it
// stands, and the dashboard states their absence rather than drawing a
// plausible zero:
//
//   - **the time of the last message.** Nothing between the bus and the
//     Companion reports it, so the row says so rather than drawing a plausible
//     zero. The *state* beside it is no longer in that category: since #108
//     `GET /api/bridges` carries each bridge's own answer about the logins it
//     holds, read from its `whoami`, and [`bridgeRows`] reads that. It used to
//     read the last login *process* — which meant a live WhatsApp link showed
//     as never connected as soon as the Gateway had been restarted.
//   - **a message count.** Nothing between the bus and the Companion counts
//     messages; the Gateway exposes no such read. The pending-decision count
//     beside it *is* real (#54), and is a count of people waiting rather than
//     of messages.
//   - **persona activity.** Hermes is not implemented (#21–#25): only its test
//     harness has landed. An activated persona therefore produces nothing, and
//     [`personaRows`] carries that as a fact about the row, not as a footnote.

import type { components } from '$lib/api/schema';
import {
	bridgeOf,
	connectionQuery,
	labelFor,
	ofKind,
	type Connection
} from '$lib/connections/registry';
import type { IconName } from '$lib/icons';
import type { MessageKey } from '$lib/i18n';
import { cardFor, manageRouteFor } from '$lib/networks/catalogue';
import { connectionOf } from '$lib/networks/connection';

export type ConfiguredBridge = components['schemas']['ConfiguredBridge'];
export type ConsentEntry = components['schemas']['ConsentStateEntry'];
export type Device = components['schemas']['Device'];
export type PortalMove = components['schemas']['PortalMove'];

/**
 * What a bridge row says — the state of the **link** the bridge holds, in the
 * five words this screen has always used for it.
 *
 * The values are unchanged; where they are read from is not. They used to
 * describe the last login *process* the Gateway had in memory, so a bridge
 * with a live session read `never` the moment the Gateway restarted (#108).
 * They now describe what the bridge itself reports, translated from the
 * contract's vocabulary by [`bridgeStateOf`].
 */
export type BridgeState =
	/** The bridge is connected to the network with this account. */
	| 'connected'
	/** Coming up, backfilling, or reconnecting by itself. Nothing to do. */
	| 'connecting'
	/**
	 * The network no longer accepts this session — the wireframe's amber
	 * "Your WhatsApp session expired". This is what a session revoked from
	 * the user's own phone reports.
	 */
	| 'expired'
	/** The bridge holds a login and the link is down for another reason. */
	| 'failed'
	/** No account is linked on this bridge. */
	| 'never'
	/**
	 * The bridge could not be reached, so its state is genuinely not known.
	 * Deliberately **not** folded into `never`: telling a user with a working
	 * link that nothing is connected is the defect this screen was part of.
	 */
	| 'unknown';

export interface BridgeRow {
	/** The connection this row is (#272): the perimeter, keyed and linked by its id. */
	readonly connectionId: string;
	/** Which account, when the kind has more than one; `null` when it is the only one. */
	readonly label: string | null;
	readonly bridgeId: string;
	readonly network: string;
	readonly state: BridgeState;
	/** Green, amber or red, as the wireframe's dot. */
	readonly tone: Tone;
	/**
	 * When a message last arrived on this network, or `null` — which is what
	 * it always is in v0.1: no component reports it (#56).
	 */
	readonly lastMessageAt: string | null;
	/** When the login this row reflects was started, for the feed. */
	readonly since: string | null;
	/** Where the user goes to fix or manage it. */
	readonly route: string;
}

export type Tone = 'ok' | 'warn' | 'bad' | 'idle';

export interface PersonaRow {
	readonly persona: string;
	/** The networks the persona is *granted* on, sorted. Empty when paused. */
	readonly networks: readonly string[];
	/** Every network the user has ever decided on for this persona, sorted. */
	readonly decidedNetworks: readonly string[];
	/** The connections the persona is active on (#272): what a pause revokes. */
	readonly connections: readonly string[];
	/** Every connection ever decided about for this persona: what a re-activation grants. */
	readonly decidedConnections: readonly string[];
	readonly active: boolean;
	/** True once any decision has been recorded about it. */
	readonly decided: boolean;
	readonly lastDecisionAt: string | null;
}

export type OverallHealth = 'ok' | 'attention' | 'setup';

export interface ActivityEntry {
	readonly id: string;
	readonly icon: IconName;
	readonly tone: Tone;
	/** ISO 8601, newest first in the feed. */
	readonly at: string;
	readonly messageKey: MessageKey;
	/**
	 * Interpolations for the message. Networks, states, bridge ids and device
	 * names only — never a contact's identity. See the module note.
	 */
	readonly values: Record<string, string>;
}

/** The wireframe's "last 10 events". */
export const FEED_LENGTH = 10;

/**
 * One row per configured bridge, saying what the **bridge** says.
 *
 * `GET /api/bridges` carries a `connection` per bridge, read live from that
 * bridge's own `whoami`: the logins it holds and the state of each. That is
 * the link, it lives in the bridge, and neither a login in flight nor a
 * restart of the Gateway can change it. This row reads that and nothing else.
 *
 * What it used to read was `bridge.login` — the last login *process* in the
 * Gateway's memory — which is why this screen and the networks picker could
 * both report a live WhatsApp session as never connected (#108).
 */
/**
 * One row per connection a bridge carries (#272): the registry says which
 * connections there are, and each one's bridge is found by the id it names
 * — never by matching networks, which is how two accounts of one kind used
 * to collapse into one row. A connection no bridge carries (Matrix) is not a
 * bridge row; a bridge no connection names is not one either, and the
 * Gateway says so at startup.
 */
export function bridgeRows(
	registry: readonly Connection[],
	bridges: readonly ConfiguredBridge[]
): BridgeRow[] {
	return registry.flatMap((entry) => {
		const bridge = bridgeOf(entry, bridges);
		if (bridge === null) {
			return [];
		}
		const connection = connectionOf(bridge);
		const state = bridgeStateOf(bridge);
		const card = cardFor(entry.kind);
		const siblings = ofKind(registry, entry.kind);
		const href = card === undefined ? null : `${card.route}${connectionQuery(entry, siblings)}`;
		const manage = card === undefined ? null : manageRouteFor({ card, href });
		return [
			{
				connectionId: entry.id,
				label: labelFor(entry, siblings),
				bridgeId: bridge.bridge_id,
				network: entry.kind,
				state,
				tone: bridgeTone(state),
				lastMessageAt: null,
				// When the link last changed state, as the bridge timestamped it.
				// For a connected login that is when it connected.
				since: connection.account?.since ?? null,
				// A link the user has is managed, not re-established: *Manage*
				// opens the management screen rather than starting a QR flow
				// against a working connection (#108).
				route: connection.linked && manage !== null ? manage : (href ?? `/networks/${entry.kind}`)
			}
		];
	});
}

/**
 * The contract's five states, as this screen's five words.
 *
 * `starting` and `degraded` are both `connecting`: one is a bridge coming up
 * and the other is one reconnecting by itself, and neither asks anything of
 * the user. `session_expired` is `expired`, which is what drives the amber
 * banner — and it is `BAD_CREDENTIALS` that reports it, never `LOGGED_OUT`,
 * which no mautrix bridge emits.
 *
 * `disconnected` splits: a bridge holding a login whose link is down is a
 * `failed` row the user should see, and a bridge holding none has simply
 * never been connected.
 */
function bridgeStateOf(bridge: ConfiguredBridge): BridgeState {
	const connection = connectionOf(bridge);
	switch (connection.state) {
		case 'connected':
			return 'connected';
		case 'starting':
		case 'degraded':
			return 'connecting';
		case 'session_expired':
			return 'expired';
		case 'disconnected':
			return connection.linked ? 'failed' : 'never';
		default:
			return 'unknown';
	}
}

function bridgeTone(state: BridgeState): Tone {
	switch (state) {
		case 'connected':
			return 'ok';
		case 'connecting':
			return 'idle';
		case 'expired':
			return 'warn';
		case 'failed':
			return 'bad';
		case 'never':
		case 'unknown':
			return 'idle';
	}
}

/**
 * What a login *process* was doing, for the activity feed — which is the one
 * place on this screen where it belongs.
 *
 * A login attempt is an event with an instant: "WhatsApp was connected at
 * 08:12" is a true thing to put on a timeline. What it is not is an answer to
 * "is WhatsApp connected now", which is why it is a separate function from
 * [`bridgeStateOf`] rather than the same one read twice.
 */
function loginActivityOf(login: NonNullable<ConfiguredBridge['login']>): BridgeState {
	switch (login.state) {
		case 'complete':
			return 'connected';
		case 'awaiting_input':
		case 'awaiting_remote':
			return 'connecting';
		case 'cancelled':
			return 'never';
		case 'failed':
			// `login_expired` and `login_lost` are the session the user has to
			// re-establish. Anything else is a failure to report as such
			// rather than as an expiry the user can simply retry.
			return login.error?.code === 'login_expired' || login.error?.code === 'login_lost'
				? 'expired'
				: 'failed';
		default:
			return 'never';
	}
}

/** The bridges whose session the user has to re-establish. */
export function expiredBridges(rows: readonly BridgeRow[]): BridgeRow[] {
	return rows.filter((row) => row.state === 'expired');
}

/**
 * One row per persona the user has decided anything about, plus every persona
 * the Companion knows, so a persona that was never activated still has a row
 * to activate it from.
 *
 * A persona is **active** when at least one network is `granted`. Pausing it
 * revokes every network it held, and the row keeps naming them, because
 * "paused on WhatsApp and Signal" is what the user has to be able to undo.
 */
export function personaRows(
	entries: readonly ConsentEntry[],
	known: readonly string[] = []
): PersonaRow[] {
	const byPersona = new Map<string, ConsentEntry[]>();
	for (const persona of known) {
		byPersona.set(persona, []);
	}
	for (const entry of entries) {
		if (entry.subject.type !== 'persona') {
			continue;
		}
		const rows = byPersona.get(entry.subject.id) ?? [];
		rows.push(entry);
		byPersona.set(entry.subject.id, rows);
	}
	return [...byPersona.entries()]
		.map(([persona, rows]) => {
			const active = rows.filter((row) => row.state === 'granted');
			const granted = [...new Set(active.map((row) => row.network))].sort();
			const decidedNetworks = [...new Set(rows.map((row) => row.network))].sort();
			const connections = [...new Set(active.map((row) => row.connection))].sort();
			const decidedConnections = [...new Set(rows.map((row) => row.connection))].sort();
			const lastDecisionAt = rows
				.map((row) => row.decided_at)
				.sort()
				.at(-1);
			return {
				persona,
				networks: granted,
				decidedNetworks,
				connections,
				decidedConnections,
				active: granted.length > 0,
				decided: rows.length > 0,
				lastDecisionAt: lastDecisionAt ?? null
			};
		})
		.sort((left, right) => left.persona.localeCompare(right.persona));
}

/**
 * The count for the wireframe's purple chip: contacts who have written and
 * about whom neither their own decision nor their network's default exists.
 *
 * It comes from `GET /api/contacts/pending` (#54), which projects the inbound
 * stream — so it knows about a contact who wrote before anything was ever
 * recorded about them, which a read of the decision journal cannot. The
 * summary reaching this function already has the identities stripped
 * (`$lib/dashboard/load.ts`); this is a count and can only ever be a count.
 *
 * `null` means *unknown* — the deployment projects no inbound stream, or the
 * read did not answer — and the chip is not drawn. A zero is a different
 * statement, and so is drawn as nothing rather than as "0 waiting": the user
 * has an empty inbox, which needs no chip.
 */
export function pendingDecisions(pending: { total: number } | null): number | null {
	return pending === null ? null : pending.total;
}

/**
 * The overall indicator: green when nothing needs the user, amber when
 * something does, and `setup` when the journey simply is not finished — a
 * deployment with no network connected is not unhealthy, it is new, and
 * telling a first-time user that their system is degraded would be a lie in
 * the other direction.
 */
export function overallHealth(options: {
	bridges: readonly BridgeRow[];
	personas: readonly PersonaRow[];
	gatewayReachable: boolean;
}): OverallHealth {
	if (!options.gatewayReachable) {
		return 'attention';
	}
	if (options.bridges.some((row) => row.state === 'expired' || row.state === 'failed')) {
		return 'attention';
	}
	const connected = options.bridges.some((row) => row.state === 'connected');
	const active = options.personas.some((row) => row.active);
	if (!connected || !active) {
		return 'setup';
	}
	return 'ok';
}

/**
 * The feed: operational events, newest first, capped at [`FEED_LENGTH`].
 *
 * Read the `values` of every row it can produce before adding one: a row's
 * values are interpolated into user-facing copy, and the invariant of this
 * screen is that none of them names a correspondent.
 */
export function activityFeed(options: {
	bridges: readonly ConfiguredBridge[];
	consent: readonly ConsentEntry[];
	devices: readonly Device[];
	/** The register's moves (#255); absent when the deployment keeps none. */
	moves?: readonly PortalMove[];
}): ActivityEntry[] {
	const entries: ActivityEntry[] = [];

	// A conversation's room was replaced while the user observed it (ADR
	// 0029): the register either followed the decision into the new room or
	// returned it to the chooser as a crowd. Said here because a deployment
	// that changed rooms under the user without being able to say so is one
	// whose history they cannot check. Numbers and nothing else — no room
	// name, which is a contact's name for a one-to-one conversation.
	for (const move of options.moves ?? []) {
		entries.push({
			id: `move:${move.successor}`,
			icon: 'observing',
			tone: move.followed ? 'ok' : 'idle',
			at: move.decided_at,
			messageKey: move.followed
				? 'dashboard.feed.move.followed'
				: 'dashboard.feed.move.returned',
			values: { members: String(move.members), threshold: String(move.crowd_threshold) }
		});
	}

	for (const bridge of options.bridges) {
		const login = bridge.login;
		if (login === null || login === undefined) {
			continue;
		}
		// The feed is a timeline of things that happened, so here — and only
		// here — the login process is the subject.
		const state = loginActivityOf(login);
		entries.push({
			id: `bridge:${bridge.bridge_id}:${login.started_at}:${login.state}`,
			icon: 'bridge',
			tone: bridgeTone(state),
			at: login.started_at,
			messageKey: `dashboard.feed.bridge.${state}` as MessageKey,
			// The bridge id and the network. `started_by.device_name` is the
			// user's own device, never a correspondent, and is left out only
			// because the row is about the bridge.
			values: { network: bridge.network, bridge: bridge.bridge_id }
		});
	}

	for (const entry of options.consent) {
		if (entry.subject.type === 'persona') {
			entries.push({
				id: `persona:${entry.subject.id}:${entry.network}:${entry.decision_sequence}`,
				icon: 'persona',
				tone: entry.state === 'granted' ? 'ok' : 'idle',
				at: entry.decided_at,
				messageKey:
					entry.state === 'granted'
						? 'dashboard.feed.persona.activated'
						: 'dashboard.feed.persona.paused',
				values: { persona: entry.subject.id, network: entry.network }
			});
			continue;
		}
		// A `contact` decision is an operational event too — a consent state
		// changed — but its subject id is a correspondent's Matrix ID. The row
		// says that a decision was taken, on which network, and to which
		// state. It does not say about whom, here or anywhere else on this
		// screen.
		entries.push({
			id: `consent:${entry.subject.type}:${entry.network}:${entry.decision_sequence}`,
			icon: 'consent',
			tone: entry.state === 'granted' ? 'ok' : 'idle',
			at: entry.decided_at,
			messageKey:
				entry.subject.type === 'network'
					? 'dashboard.feed.consent.network'
					: 'dashboard.feed.consent.contact',
			values: { network: entry.network, state: entry.state }
		});
	}

	for (const device of options.devices) {
		entries.push({
			id: `device:${device.id}:signed-in`,
			icon: 'device',
			tone: 'idle',
			at: isoFromUnix(device.created_unix_seconds),
			messageKey: 'dashboard.feed.device.signedIn',
			values: { device: device.name }
		});
		if (device.revoked_unix_seconds !== null && device.revoked_unix_seconds !== undefined) {
			entries.push({
				id: `device:${device.id}:revoked`,
				icon: 'device',
				tone: 'warn',
				at: isoFromUnix(device.revoked_unix_seconds),
				messageKey: 'dashboard.feed.device.revoked',
				values: { device: device.name }
			});
		}
	}

	return entries.sort((left, right) => right.at.localeCompare(left.at)).slice(0, FEED_LENGTH);
}

function isoFromUnix(seconds: number): string {
	return new Date(seconds * 1000).toISOString();
}

/**
 * The messages the user's networks carried, or `null` when nobody counts them
 * — which is v0.1's answer. The count, never the senders (#74): this function
 * exists so that the screen has one place to read it from when a counter
 * lands, and so that "unknown" is a value the screen renders rather than a
 * zero it invents.
 */
export function messageCount(): number | null {
	return null;
}
