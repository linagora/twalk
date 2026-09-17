// The copy one QR login screen needs, as message keys.
//
// Screens 3a and 3b are the same screen with different words: the same
// lifecycle (waiting, expired, scanning, success, failure), the same polling,
// the same accessibility text — and a different disclosure, different
// instructions and a different tone. So the shared component takes its words as
// keys rather than building them from the network's name, which keeps every key
// a literal the i18n catalogue's type checks, and keeps a missing translation a
// compile error rather than a key printed on screen.

import type { MessageKey } from '$lib/i18n';

/** The amber card a user must confirm before the code is shown. */
export interface Disclosure {
	readonly title: MessageKey;
	readonly body: MessageKey;
	readonly confirm: MessageKey;
	readonly learnMore: MessageKey;
	readonly learnMoreHref: string;
	/** Where the dismissal is remembered, per device. */
	readonly storageKey: string;
}

export interface QrScreenCopy {
	readonly network: string;
	readonly title: MessageKey;
	readonly caption: MessageKey;
	readonly disclosure: Disclosure | null;
	/** The numbered steps under the code. */
	readonly steps: readonly MessageKey[];
	/** The calm informational chip under the steps, when there is one. */
	readonly note: MessageKey | null;
	/** The screen-reader alternative for the code itself. */
	readonly qrAlt: MessageKey;
	readonly success: MessageKey;
	readonly troubleshooting: readonly MessageKey[];
}

export const WHATSAPP: QrScreenCopy = {
	network: 'whatsapp',
	title: 'qr.whatsapp.title',
	caption: 'qr.whatsapp.caption',
	disclosure: {
		title: 'qr.whatsapp.disclosure.title',
		body: 'qr.whatsapp.disclosure.body',
		confirm: 'qr.disclosure.confirm',
		learnMore: 'qr.disclosure.learnMore',
		learnMoreHref:
			'https://github.com/linagora/twalk/blob/main/docs/wireframes/companion-v0.1.md#screen-3a--whatsapp-login-qr-code',
		storageKey: 'twalk.disclosure.whatsapp'
	},
	steps: ['qr.whatsapp.step1', 'qr.whatsapp.step2', 'qr.whatsapp.step3'],
	note: null,
	qrAlt: 'qr.whatsapp.alt',
	success: 'qr.whatsapp.success',
	troubleshooting: ['qr.whatsapp.trouble1', 'qr.whatsapp.trouble2', 'qr.whatsapp.trouble3']
};

export const SIGNAL: QrScreenCopy = {
	network: 'signal',
	title: 'qr.signal.title',
	caption: 'qr.signal.caption',
	// No disclosure: Signal officially supports secondary devices, so there is
	// no ban risk to disclose and an amber card would be theatre.
	disclosure: null,
	steps: ['qr.signal.step1', 'qr.signal.step2', 'qr.signal.step3', 'qr.signal.step4'],
	note: 'qr.signal.note',
	qrAlt: 'qr.signal.alt',
	success: 'qr.signal.success',
	troubleshooting: ['qr.signal.trouble1', 'qr.signal.trouble2']
};
