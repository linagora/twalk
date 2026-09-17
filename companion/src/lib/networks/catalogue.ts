// The networks screen 3 offers, and what the Companion knows about each one
// without asking the Gateway anything.
//
// A **network** is what the user experiences — WhatsApp, Signal, SMS, Matrix —
// and never a bridge name (CONTEXT.md). `GET /api/bridges` maps the two: it
// reports one row per configured bridge instance, each carrying the network it
// serves. So this table is the user-facing half (copy, icon, route, milestone,
// what the platform allows) and the Gateway's list is the deployment half
// (which of them this deployment can actually connect, and under which
// `bridge_id`). Screen 3 joins them.
//
// Matrix is in the table and in no bridge list: the bring-your-own-account path
// (ADR 0009) needs no bridge, so its card is always active and its screen talks
// to the homeserver and to `/api/bootstrap/rooms`.

import type { IconName } from '$lib/icons';
import type { MessageKey } from '$lib/i18n';

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
		subtitleKey: 'network.comingSoon',
		milestone: 'v0.2',
		preview: false,
		needsBridge: true,
		androidOnly: false,
		route: null
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

/** One row of `GET /api/bridges`, reduced to what screen 3 needs from it. */
export interface BridgeRow {
	readonly bridge_id: string;
	readonly network: string;
	readonly login: { readonly state: string } | null;
}

/** Why a card cannot be tapped, or `null` when it can. */
export type CardBlock = 'coming-soon' | 'no-bridge' | 'ios';

/** A card joined with what this deployment and this browser allow. */
export interface CardState {
	readonly card: NetworkCard;
	/** The bridge instance serving it, when the deployment has one. */
	readonly bridgeId: string | null;
	/** The login already completed on it: the wireframe's green check. */
	readonly connected: boolean;
	readonly blockedBy: CardBlock | null;
}

/**
 * Screen 3's grid: every card, with the deployment's bridges and the platform
 * folded in.
 *
 * `bridges` is `GET /api/bridges`, or an empty list when the call failed —
 * a Gateway that cannot answer must not silently turn every card into "not
 * configured", so the caller passes `bridgesKnown: false` and the screen says
 * so instead. Pure, so `catalogue.test.ts` covers every combination.
 */
export function gridFor(options: {
	bridges: readonly BridgeRow[];
	bridgesKnown: boolean;
	ios: boolean;
}): CardState[] {
	return NETWORK_CARDS.map((card) => {
		const bridge = options.bridges.find((row) => row.network === card.network) ?? null;
		const connected = bridge?.login?.state === 'complete';
		let blockedBy: CardBlock | null = null;
		if (card.milestone === 'v0.2') {
			blockedBy = 'coming-soon';
		} else if (card.androidOnly && options.ios) {
			blockedBy = 'ios';
		} else if (card.needsBridge && options.bridgesKnown && bridge === null) {
			blockedBy = 'no-bridge';
		}
		return {
			card,
			bridgeId: bridge?.bridge_id ?? null,
			connected,
			blockedBy
		};
	});
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
