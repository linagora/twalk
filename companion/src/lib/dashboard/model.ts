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
//   - **live bridge state and last message time.** `GET /api/bridges` reports
//     the login a bridge holds, which is the last login *process*, not a
//     heartbeat. #56 has landed the *producing* half — each bridge pushes its
//     connection state to the Gateway's webhook and the Gateway publishes
//     `bridge.status.changed.v1` — but it added no read for the Companion, so
//     nothing on this origin can be asked what state a bridge is in now. That
//     read, or the merged event stream the wireframe names, is what turns this
//     row's dot into the wireframe's dot.
//   - **a message count.** Nothing between the bus and the Companion counts
//     messages; the Gateway exposes no such read. The pending-decision count
//     beside it *is* real (#54), and is a count of people waiting rather than
//     of messages.
//   - **persona activity.** Hermes is not implemented (#21–#25): only its test
//     harness has landed. An activated persona therefore produces nothing, and
//     [`personaRows`] carries that as a fact about the row, not as a footnote.

import type { components } from '$lib/api/schema';
import type { IconName } from '$lib/icons';
import type { MessageKey } from '$lib/i18n';

export type ConfiguredBridge = components['schemas']['ConfiguredBridge'];
export type ConsentEntry = components['schemas']['ConsentStateEntry'];
export type Device = components['schemas']['Device'];

/** What a bridge row says, derived from the login the Gateway holds for it. */
export type BridgeState =
	/** A login completed: as connected as the Gateway can tell. */
	| 'connected'
	/** A login is in flight right now. */
	| 'connecting'
	/** The last login expired or was lost — the wireframe's amber state. */
	| 'expired'
	/** The last login failed for another reason. */
	| 'failed'
	/** Nothing was ever connected on this bridge. */
	| 'never';

export interface BridgeRow {
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
 * One row per configured bridge.
 *
 * The state is read off the login the Gateway holds, and the row is honest
 * about what that is: `GET /api/bridges` "reads configuration and the
 * Gateway's own memory: no bridge is contacted". A bridge that is down but
 * whose last login completed therefore reads `connected` here — which is why
 * the screen's own copy names this as the login's state and not as a
 * heartbeat. The live state exists on the bus (#56 publishes it) and has no
 * read on this origin; when one lands, it is this function that reads it.
 */
export function bridgeRows(bridges: readonly ConfiguredBridge[]): BridgeRow[] {
	return bridges.map((bridge) => {
		const state = bridgeStateOf(bridge);
		return {
			bridgeId: bridge.bridge_id,
			network: bridge.network,
			state,
			tone: bridgeTone(state),
			lastMessageAt: null,
			since: bridge.login?.started_at ?? null,
			route: `/networks/${bridge.network}`
		};
	});
}

function bridgeStateOf(bridge: ConfiguredBridge): BridgeState {
	const login = bridge.login;
	if (login === null || login === undefined) {
		return 'never';
	}
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
			// re-establish — the wireframe's "Your WhatsApp session expired"
			// banner. Anything else is a failure to report as such rather than
			// as an expiry the user can simply retry.
			return login.error?.code === 'login_expired' || login.error?.code === 'login_lost'
				? 'expired'
				: 'failed';
		default:
			return 'never';
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
			return 'idle';
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
			const granted = rows
				.filter((row) => row.state === 'granted')
				.map((row) => row.network)
				.sort();
			const decidedNetworks = [...new Set(rows.map((row) => row.network))].sort();
			const lastDecisionAt = rows
				.map((row) => row.decided_at)
				.sort()
				.at(-1);
			return {
				persona,
				networks: granted,
				decidedNetworks,
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
}): ActivityEntry[] {
	const entries: ActivityEntry[] = [];

	for (const bridge of options.bridges) {
		const login = bridge.login;
		if (login === null || login === undefined) {
			continue;
		}
		const state = bridgeStateOf(bridge);
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
