// The kinds of connection screen 3 offers, and what the Companion knows about
// each one without asking the Gateway anything.
//
// A **network** is what the user experiences — WhatsApp, Signal, SMS, Matrix —
// and never a bridge name (CONTEXT.md). It is the *kind* of a connection, not
// its identity (ADR 0033): this table is the user-facing half of a kind (copy,
// icon, route, milestone, what the platform allows), and the Gateway's
// registry (`GET /api/connections`) is the deployment half — which
// connections there are, and under which `bridge_id` each is carried. Screen
// 3 joins them: **one card per connection**, and a kind with no connection
// keeps one card, blocked. A bridge is found by the id its connection names,
// never by matching networks (#272).
//
// Matrix is in the table and in no bridge list: the bring-your-own-account path
// (ADR 0009) needs no bridge, its connection is in every registry, and its
// screen talks to the homeserver and to `/api/bootstrap/rooms`.
//
// What a card says about being connected comes from `connection.ts`, which
// reads the bridge's own answer. It must never come from the login *process*
// the Gateway may have in flight: that was the defect in #108, where starting
// a login, cancelling one or restarting the Gateway each made a live WhatsApp
// link read as no link at all.

import type { IconName } from '$lib/icons';
import type { MessageKey } from '$lib/i18n';
import { bridgeOf, labelFor, linkTo, ofKind, type Connection } from '$lib/connections/registry';
import {
	connectionOf,
	isConnected,
	type ConfiguredBridge,
	type NetworkConnection
} from './connection';

/** The milestone a card belongs to. v0.2 cards are shown, never interactive. */
export type Milestone = 'v0.1' | 'v0.2';

export interface NetworkCard {
	/** The `network` value the Gateway reports, and the route segment. */
	readonly network: string;
	readonly icon: IconName;
	readonly titleKey: MessageKey;
	readonly subtitleKey: MessageKey;
	readonly milestone: Milestone;
	/** The wireframes' discreet "Preview" badge: the SMS path only. */
	readonly preview: boolean;
	/**
	 * Whether this network needs a bridge the deployment configured. `false`
	 * for Matrix, whose card is active whatever the Gateway lists.
	 */
	readonly needsBridge: boolean;
	/**
	 * Unusable on an iOS-only device. The SMS preview goes through Google
	 * Messages on an Android phone, so an iPhone cannot feed it — the card is
	 * greyed with a tooltip rather than removed, because a user with both
	 * devices can still use it from the Android one.
	 */
	readonly androidOnly: boolean;
	/** Where the card leads. `null` for a v0.2 card, which leads nowhere. */
	readonly route: string | null;
}

/** The order of the wireframe's grid: the four v0.1 cards, then the two v0.2. */
export const NETWORK_CARDS: readonly NetworkCard[] = [
	{
		network: 'whatsapp',
		icon: 'whatsapp',
		titleKey: 'network.whatsapp.name',
		subtitleKey: 'network.whatsapp.subtitle',
		milestone: 'v0.1',
		preview: false,
		needsBridge: true,
		androidOnly: false,
		route: '/networks/whatsapp'
	},
	{
		network: 'signal',
		icon: 'signal',
		titleKey: 'network.signal.name',
		subtitleKey: 'network.signal.subtitle',
		milestone: 'v0.1',
		preview: false,
		needsBridge: true,
		androidOnly: false,
		route: '/networks/signal'
	},
	{
		network: 'sms',
		icon: 'sms',
		titleKey: 'network.sms.name',
		subtitleKey: 'network.sms.subtitle',
		milestone: 'v0.1',
		preview: true,
		needsBridge: true,
		androidOnly: true,
		route: '/networks/sms'
	},
	{
		network: 'matrix',
		icon: 'matrix',
		titleKey: 'network.matrix.name',
		subtitleKey: 'network.matrix.subtitle',
		milestone: 'v0.1',
		preview: false,
		needsBridge: false,
		androidOnly: false,
		route: '/networks/matrix'
	},
	{
		network: 'telegram',
		icon: 'telegram',
		titleKey: 'network.telegram.name',
		subtitleKey: 'network.telegram.subtitle',
		// Was a v0.2 card leading nowhere, which stopped being true the day the
		// reference deployment started running `mautrix-telegram`: the bridge
		// was up, healthy and advertising four login flows while the card still
		// said "coming soon". A deployment that *can* connect a network and a
		// Companion that says it cannot are two states behind one appearance,
		// and the card is what the user reads.
		milestone: 'v0.1',
		preview: false,
		needsBridge: true,
		androidOnly: false,
		route: '/networks/telegram'
	},
	{
		network: 'discord',
		icon: 'discord',
		titleKey: 'network.discord.name',
		subtitleKey: 'network.comingSoon',
		milestone: 'v0.2',
		preview: false,
		needsBridge: true,
		androidOnly: false,
		route: null
	}
];

export function cardFor(network: string): NetworkCard | undefined {
	return NETWORK_CARDS.find((card) => card.network === network);
}

/**
 * Where *Manage* leads for a card that has a link to manage — naming the
 * connection when its kind has several, as the card's own link does.
 *
 * `null` for a kind with no bridge behind it: Matrix reaches Twalk by the
 * Sensor being invited into the user's own rooms (ADR 0009), so there is no
 * login to disconnect and no management screen to open. Its card keeps
 * leading to its own screen.
 */
export function manageRouteFor(state: Pick<CardState, 'card' | 'href'>): string | null {
	if (!state.card.needsBridge || state.card.route === null || state.href === null) {
		return null;
	}
	const [path, query] = state.href.split('?');
	return `${path}/manage${query === undefined ? '' : `?${query}`}`;
}

/**
 * One row of `GET /api/bridges`.
 *
 * The whole row, not a reduction of it: the two members that matter here have
 * confusable names, and a hand-written subset is how the wrong one got read.
 * `connection` is the bridge's own answer about the link it holds;
 * `login` is a login *process* in the Gateway's memory and is not this
 * screen's business at all.
 */
export type BridgeRow = ConfiguredBridge;

/** Why a card cannot be tapped, or `null` when it can. */
export type CardBlock = 'coming-soon' | 'no-bridge' | 'ios';

/** A card joined with what this deployment and this browser allow. */
export interface CardState {
	readonly card: NetworkCard;
	/**
	 * The connection this card is (#272), or `null` for a kind the registry
	 * has none of — the one card such a kind keeps, blocked.
	 */
	readonly connection: Connection | null;
	/** Distinct per card: the connection's id, or the kind's when it has none. */
	readonly key: string;
	/** Which account, when the kind has more than one; `null` when it is the only one. */
	readonly label: string | null;
	/** Where the card leads — the kind's route, naming the connection when the kind has several. */
	readonly href: string | null;
	/** The bridge instance carrying it, by the id the connection names. */
	readonly bridgeId: string | null;
	/**
	 * The link the bridge holds, read from the bridge's `connection` member
	 * and from nothing else (#108). Survives a login being started, cancelled,
	 * or the Gateway being restarted, because none of those is a thing that
	 * can unlink an account. Named `link` here since #272, because
	 * `connection` is the perimeter (ADR 0033) and the two are not one thing.
	 */
	readonly link: NetworkConnection;
	/** The wireframe's green check: `connection.state === 'connected'`. */
	readonly connected: boolean;
	/**
	 * Whether the bridge holds a login at all — what decides between
	 * *Connect* and *Manage*. An expired session is still a link, and what
	 * the user wants for it is the management screen, not a fresh QR flow.
	 */
	readonly linked: boolean;
	readonly blockedBy: CardBlock | null;
}

/**
 * Screen 3's grid: one card per connection of each catalogued kind, with the
 * deployment's bridges and the platform folded in.
 *
 * `connections` is `GET /api/connections` and `bridges` is `GET /api/bridges`,
 * each an empty list when its call failed — a Gateway that cannot answer must
 * not silently turn every card into "not configured", so the caller passes
 * `connectionsKnown: false` or `bridgesKnown: false` and the screen says so
 * instead. Pure, so `catalogue.test.ts` covers every combination, the
 * two-accounts shape included.
 */
export function gridFor(options: {
	connections: readonly Connection[];
	connectionsKnown: boolean;
	bridges: readonly BridgeRow[];
	bridgesKnown: boolean;
	ios: boolean;
}): CardState[] {
	return NETWORK_CARDS.flatMap((card) => {
		const siblings = ofKind(options.connections, card.network);
		// A kind with no connection keeps one card: blocked when the registry
		// was read and says so, tappable when nobody could be asked.
		const each: (Connection | null)[] = siblings.length === 0 ? [null] : siblings;
		return each.map((connection) => cardState(card, connection, siblings, options));
	});
}

function cardState(
	card: NetworkCard,
	connection: Connection | null,
	siblings: readonly Connection[],
	options: {
		connectionsKnown: boolean;
		bridges: readonly BridgeRow[];
		bridgesKnown: boolean;
		ios: boolean;
	}
): CardState {
	const noConnection = connection === null && options.connectionsKnown;
	const bridge = connection === null ? null : bridgeOf(connection, options.bridges);
	const link = connectionOf(bridge);
	let blockedBy: CardBlock | null = null;
	if (card.milestone === 'v0.2') {
		blockedBy = 'coming-soon';
	} else if (card.androidOnly && options.ios) {
		blockedBy = 'ios';
	} else if (card.needsBridge && (noConnection || (options.bridgesKnown && bridge === null))) {
		blockedBy = 'no-bridge';
	}
	return {
		card,
		connection,
		key: connection?.id ?? card.network,
		label: connection === null ? null : labelFor(connection, siblings),
		href: card.route === null || noConnection ? null : linkTo(card.route, connection, siblings),
		bridgeId: bridge?.bridge_id ?? null,
		link,
		connected: isConnected(link),
		linked: link.linked,
		blockedBy
	};
}

/**
 * Whether this is an iOS user agent, for the SMS card's greyed state.
 *
 * iPadOS 13+ claims to be a Mac, so the touch-point check is what catches an
 * iPad; `MSStream` is the old Windows Phone tell that used to match `iPad` in
 * its UA. Deliberately a *hint*: getting it wrong greys a card and shows a
 * tooltip that explains why, which is recoverable, and the screen itself still
 * says plainly that the preview needs an Android phone.
 */
export function looksLikeIos(agent: string, touchPoints: number, platform: string): boolean {
	if (/MSStream/i.test(agent)) {
		return false;
	}
	if (/iPhone|iPad|iPod/i.test(agent)) {
		return true;
	}
	return /Mac/i.test(platform) && touchPoints > 1;
}
