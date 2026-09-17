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
import Bug from '@lucide/svelte/icons/bug';
import Check from '@lucide/svelte/icons/check';
import ChevronLeft from '@lucide/svelte/icons/chevron-left';
import CircleAlert from '@lucide/svelte/icons/circle-alert';
import CircleCheck from '@lucide/svelte/icons/circle-check';
import CircleX from '@lucide/svelte/icons/circle-x';
import ClipboardCheck from '@lucide/svelte/icons/clipboard-check';
import CloudDownload from '@lucide/svelte/icons/cloud-download';
import Copy from '@lucide/svelte/icons/copy';
import Cpu from '@lucide/svelte/icons/cpu';
import Database from '@lucide/svelte/icons/database';
import ExternalLink from '@lucide/svelte/icons/external-link';
import FileDown from '@lucide/svelte/icons/file-down';
import Globe from '@lucide/svelte/icons/globe';
import Info from '@lucide/svelte/icons/info';
import KeyRound from '@lucide/svelte/icons/key-round';
import Languages from '@lucide/svelte/icons/languages';
import LockKeyhole from '@lucide/svelte/icons/lock-keyhole';
import MonitorSmartphone from '@lucide/svelte/icons/monitor-smartphone';
import RefreshCw from '@lucide/svelte/icons/refresh-cw';
import ShieldCheck from '@lucide/svelte/icons/shield-check';
import Smartphone from '@lucide/svelte/icons/smartphone';
import TriangleAlert from '@lucide/svelte/icons/triangle-alert';

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
	diagnostics: Bug
} as const;

/** A name the app may ask [`Icon`](./Icon.svelte) to draw. */
export type IconName = keyof typeof icons;

/**
 * The wireframes' two icon contexts: 24 px by default, 20 px where things are
 * dense. The pixel values live in the design tokens (`--icon-size`,
 * `--icon-size-dense`) and are applied as CSS; see `./Icon.svelte`.
 */
export type IconSize = 'default' | 'dense';
