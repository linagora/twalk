// The Companion's icon set — the *only* file in the app that imports from an
// icon library.
//
// The wireframes specify Lucide at 24 px, 20 px in dense contexts, and the
// Twake product family has its own set (`@linagora/twake-icons`) which is not
// consumable here yet. So the app never names a Lucide icon: it names what the
// icon *means* ('storage', 'secure-context', 'copy'), and this table maps that
// meaning onto Lucide. Swapping the set is then editing this one table, not
// grepping the app for `lucide`.
//
// Add a row when a screen needs a symbol the table has no name for; never
// import `@lucide/svelte` anywhere else.

import ArrowRight from '@lucide/svelte/icons/arrow-right';
import AtSign from '@lucide/svelte/icons/at-sign';
import Ban from '@lucide/svelte/icons/ban';
import BotMessageSquare from '@lucide/svelte/icons/bot-message-square';
import Bug from '@lucide/svelte/icons/bug';
import Check from '@lucide/svelte/icons/check';
import ChevronLeft from '@lucide/svelte/icons/chevron-left';
import CircleAlert from '@lucide/svelte/icons/circle-alert';
import CircleCheck from '@lucide/svelte/icons/circle-check';
import CircleX from '@lucide/svelte/icons/circle-x';
import ClipboardCheck from '@lucide/svelte/icons/clipboard-check';
import CircleSlash from '@lucide/svelte/icons/circle-slash';
import CloudDownload from '@lucide/svelte/icons/cloud-download';
import Cookie from '@lucide/svelte/icons/cookie';
import Copy from '@lucide/svelte/icons/copy';
import Cpu from '@lucide/svelte/icons/cpu';
import Database from '@lucide/svelte/icons/database';
import ExternalLink from '@lucide/svelte/icons/external-link';
import FileDown from '@lucide/svelte/icons/file-down';
import Gamepad2 from '@lucide/svelte/icons/gamepad-2';
import Globe from '@lucide/svelte/icons/globe';
import Hash from '@lucide/svelte/icons/hash';
import Mail from '@lucide/svelte/icons/mail';
import Inbox from '@lucide/svelte/icons/inbox';
import Info from '@lucide/svelte/icons/info';
import KeyRound from '@lucide/svelte/icons/key-round';
import Languages from '@lucide/svelte/icons/languages';
import LockKeyhole from '@lucide/svelte/icons/lock-keyhole';
import LogOut from '@lucide/svelte/icons/log-out';
import MessageCircle from '@lucide/svelte/icons/message-circle';
import MessageSquareLock from '@lucide/svelte/icons/message-square-lock';
import MessageSquareText from '@lucide/svelte/icons/message-square-text';
import MonitorSmartphone from '@lucide/svelte/icons/monitor-smartphone';
import QrCode from '@lucide/svelte/icons/qr-code';
import RefreshCw from '@lucide/svelte/icons/refresh-cw';
import Pause from '@lucide/svelte/icons/pause';
import Play from '@lucide/svelte/icons/play';
import Plug from '@lucide/svelte/icons/plug';
import Send from '@lucide/svelte/icons/send';
import Settings from '@lucide/svelte/icons/settings';
import ShieldCheck from '@lucide/svelte/icons/shield-check';
import Smartphone from '@lucide/svelte/icons/smartphone';
import CircleQuestionMark from '@lucide/svelte/icons/circle-question-mark';
import Eye from '@lucide/svelte/icons/eye';
import Megaphone from '@lucide/svelte/icons/megaphone';
import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
import Users from '@lucide/svelte/icons/users';
import UsersRound from '@lucide/svelte/icons/users-round';

/**
 * Every symbol the Companion draws, named by what it means. The values are the
 * current set's components and are an implementation detail of this module.
 */
export const icons = {
	// Navigation and actions
	back: ChevronLeft,
	continue: ArrowRight,
	copy: Copy,
	copied: ClipboardCheck,
	reload: RefreshCw,
	download: FileDown,
	'external-link': ExternalLink,
	language: Languages,

	// Status
	ok: CircleCheck,
	warning: TriangleAlert,
	error: CircleX,
	attention: CircleAlert,
	info: Info,
	check: Check,

	// The capability gate's causes, one per row it can draw
	'secure-context': ShieldCheck,
	webassembly: Cpu,
	storage: Database,
	'service-worker': CloudDownload,
	'tab-lock': LockKeyhole,

	// Domain things
	domain: Globe,
	'recovery-key': KeyRound,
	device: MonitorSmartphone,
	phone: Smartphone,
	diagnostics: Bug,
	'qr-code': QrCode,
	manage: Settings,
	cookie: Cookie,
	unavailable: Ban,

	// The dashboard of screen 5
	persona: BotMessageSquare,
	bridge: Plug,
	consent: Inbox,
	pause: Pause,
	activate: Play,
	revoke: LogOut,
	locked: CircleSlash,

	// The networks of screen 3. Lucide dropped its brand marks, so each card
	// gets the symbol that says what the network *is* rather than a logo we
	// are not licensed to draw; the card's own name does the identifying.
	whatsapp: MessageCircle,
	signal: MessageSquareLock,
	sms: MessageSquareText,
	matrix: Hash,
	telegram: Send,
	discord: Gamepad2,
	// A mailbox is a network (ADR 0033); its card lands with the collector.
	email: Mail,
	account: AtSign,

	// The conversation chooser of #143, one per kind of conversation the
	// network's own identifier names — plus the eye, which is what "the Sensor
	// is reading this one" looks like.
	'one-to-one': AtSign,
	group: Users,
	community: UsersRound,
	broadcast: Megaphone,
	'kind-unstated': CircleQuestionMark,
	observing: Eye
} as const;

/** A name the app may ask [`Icon`](./Icon.svelte) to draw. */
export type IconName = keyof typeof icons;

/**
 * The wireframes' two icon contexts: 24 px by default, 20 px where things are
 * dense. The pixel values live in the design tokens (`--icon-size`,
 * `--icon-size-dense`) and are applied as CSS; see `./Icon.svelte`.
 */
export type IconSize = 'default' | 'dense';
