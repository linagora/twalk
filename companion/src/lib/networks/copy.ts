// The copy one bridge-login screen needs, as message keys.
//
// Screens 3a, 3b and 3c are one screen with different words: the same
// lifecycle (waiting, expired, scanning, refused, success, failure), the same
// polling, the same accessibility text — and a different disclosure, different
// instructions and a different tone. So the shared component takes its words as
// keys rather than building them from the network's name, which keeps every key
// a literal the i18n catalogue's type checks, and keeps a missing translation a
// compile error rather than a key printed on screen.
//
// Since ADR 0030 a screen also carries **its steps' words** (`steps`), keyed on
// the bridge's own step ids. That is where #57's four requirements live: which
// cookies, where to get them, why a private window, and that Device Bound
// Session Credentials must be off. They come from no bridge, they are
// acceptance criteria, and `step-copy.ts` is what keeps them attached to the
// step they explain.

import type { MessageKey } from '$lib/i18n';

import { section, type StepOverride } from './step-copy';

/** The amber card a user must confirm before the login starts. */
export interface Disclosure {
	readonly title: MessageKey;
	readonly body: MessageKey;
	readonly confirm: MessageKey;
	readonly learnMore: MessageKey;
	readonly learnMoreHref: string;
	/** Where the dismissal is remembered, per device. */
	readonly storageKey: string;
}

export interface LoginScreenCopy {
	readonly network: string;
	readonly title: MessageKey;
	/** The badge beside the title, for a path this project calls a preview. */
	readonly badge: MessageKey | null;
	readonly caption: MessageKey | null;
	/** The calm chip above the login, stating what the path needs of the user. */
	readonly intro: MessageKey | null;
	readonly disclosure: Disclosure | null;
	/** Which of the bridge's flows to ask for. */
	readonly prefer: 'qr' | 'cookies';
	/** The numbered steps drawn beside a blocking step. */
	readonly steps: readonly MessageKey[];
	/** The calm informational chip under those steps, when there is one. */
	readonly note: MessageKey | null;
	/** The screen-reader alternative for a drawn code, when this path draws one. */
	readonly qrAlt: MessageKey | null;
	readonly success: MessageKey;
	/** A sentence after the success line, when the path has one to add. */
	readonly successNote: MessageKey | null;
	/** Replaces the generic "the bridge refused this login" failure. */
	readonly failure: MessageKey | null;
	/** This project's words for the bridge's own steps, keyed on their ids. */
	readonly stepCopy: readonly StepOverride[];
	readonly troubleshooting: readonly MessageKey[];
}

export const WHATSAPP: LoginScreenCopy = {
	network: 'whatsapp',
	title: 'qr.whatsapp.title',
	badge: null,
	caption: 'qr.whatsapp.caption',
	intro: null,
	disclosure: {
		title: 'qr.whatsapp.disclosure.title',
		body: 'qr.whatsapp.disclosure.body',
		confirm: 'qr.disclosure.confirm',
		learnMore: 'qr.disclosure.learnMore',
		learnMoreHref:
			'https://github.com/linagora/twalk/blob/main/docs/wireframes/companion-v0.1.md#screen-3a--whatsapp-login-qr-code',
		storageKey: 'twalk.disclosure.whatsapp'
	},
	prefer: 'qr',
	steps: ['qr.whatsapp.step1', 'qr.whatsapp.step2', 'qr.whatsapp.step3'],
	note: null,
	qrAlt: 'qr.whatsapp.alt',
	success: 'qr.whatsapp.success',
	successNote: null,
	failure: null,
	// Nothing to add to WhatsApp's own steps: its QR instructions are the
	// network's wording for its own screen, which is exactly what a bridge is
	// good at. A two-factor password step — which a QR login can interject —
	// asks for one thing and needs no explanation this project could improve.
	stepCopy: [],
	troubleshooting: ['qr.whatsapp.trouble1', 'qr.whatsapp.trouble2', 'qr.whatsapp.trouble3']
};

export const SIGNAL: LoginScreenCopy = {
	network: 'signal',
	title: 'qr.signal.title',
	badge: null,
	caption: 'qr.signal.caption',
	intro: null,
	// No disclosure: Signal officially supports secondary devices, so there is
	// no ban risk to disclose and an amber card would be theatre.
	disclosure: null,
	prefer: 'qr',
	steps: ['qr.signal.step1', 'qr.signal.step2', 'qr.signal.step3', 'qr.signal.step4'],
	note: 'qr.signal.note',
	qrAlt: 'qr.signal.alt',
	success: 'qr.signal.success',
	successNote: null,
	failure: null,
	stepCopy: [],
	troubleshooting: ['qr.signal.trouble1', 'qr.signal.trouble2']
};

/**
 * The Google cookie step, in this project's words rather than the bridge's
 * eleven.
 *
 * The step ids: mautrix-gmessages names its steps `fi.mau.gmessages.login.*`,
 * which is the connector's own namespace, and the stub the suite drives answers
 * `fi.mau.stub.login.cookies`. Neither is captured from a live bridge — a
 * Google Messages login needs a real account — so this is the one place in this
 * file where an id could be wrong, and the failure mode is visible rather than
 * silent: an unmatched step id draws the bridge's own words and a card saying
 * the explanation is missing (`FieldsPanel.svelte`).
 */
const GOOGLE_COOKIES: StepOverride = {
	stepIds: ['fi.mau.gmessages.login.cookies', 'fi.mau.stub.login.cookies'],
	expects: { kind: 'cookies', fieldTypes: ['cookie'] },
	// No headline of its own: the sections below *are* the instructions, and the
	// bridge's eleven words are replaced rather than printed above them.
	instructions: null,
	before: [
		section({ title: 'sms.step1.title', steps: ['sms.step1.a', 'sms.step1.b', 'sms.step1.c'] }),
		section({ title: 'sms.step2.title', body: 'sms.step2.why' }),
		// The two settings that otherwise make the whole copy useless. A warning
		// card, because they are the difference between a login that works and
		// one that fails a week later for no visible reason (#57).
		section({
			title: 'sms.step2.requirements',
			bullets: ['sms.step2.privateWindow', 'sms.step2.dbsc'],
			warn: true
		}),
		section({ steps: ['sms.step2.a', 'sms.step2.b', 'sms.step2.c'], link: true }),
		// What the seven cookies together let their holder do, read **before**
		// the paste rather than after it: it is the sentence that lets someone
		// decide not to (#57).
		section({ body: 'sms.cookies.whatTheyAre' })
	],
	submit: 'sms.cookies.submit',
	after: ['sms.cookies.neverStored'],
	refused: 'sms.failed.google'
};

export const SMS: LoginScreenCopy = {
	network: 'sms',
	title: 'sms.title',
	badge: 'networks.preview',
	caption: null,
	intro: 'sms.requirement',
	disclosure: {
		title: 'sms.disclosure.title',
		body: 'sms.disclosure.body',
		confirm: 'qr.disclosure.confirm',
		learnMore: 'qr.disclosure.learnMore',
		learnMoreHref:
			'https://github.com/linagora/twalk/blob/main/docs/architecture/adr/0004-twake-sms-companion-first-party-app.md',
		storageKey: 'twalk.disclosure.sms'
	},
	prefer: 'cookies',
	// The cookie flow's steps are the override's; nothing is drawn beside a
	// blocking step here but the emoji, which speaks for itself.
	steps: [],
	note: null,
	qrAlt: null,
	success: 'sms.success',
	successNote: 'sms.migration',
	failure: 'sms.failed.google',
	stepCopy: [GOOGLE_COOKIES],
	troubleshooting: ['sms.trouble1', 'sms.trouble2', 'sms.trouble3']
};
