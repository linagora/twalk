// The printable recovery-key sheet. Two things matter and both are asserted:
// that it is a PDF a reader will open, and that what the user came for — the
// key, and the sentence saying what is lost without it — is actually in it.

import { describe, expect, it } from 'vitest';

import { keyRows, recoveryKeyPdf } from './pdf';

const KEY = 'ABCD EFGH JKLM NPQR STUV WXYZ abcd efgh jkmn opqr stuv wxyz';

const STRINGS = {
	title: 'Twalk recovery key',
	intro: 'This sheet carries the only copy of your Twalk recovery key.',
	keyLabel: 'Your recovery key',
	warning: 'Without this key your encrypted messages cannot be read back by anyone.',
	advice: ['Keep it like a passport.', 'Do not email it.', 'Twalk never sees it.'],
	account: 'Account: @you:example.com — example.com',
	generated: 'Generated on 2026-09-18 by the Twalk Companion.'
};

function text(bytes: Uint8Array): string {
	return new TextDecoder('latin1').decode(bytes);
}

describe('recoveryKeyPdf', () => {
	const pdf = recoveryKeyPdf({ groupedKey: KEY, strings: STRINGS });
	const body = text(pdf);

	it('is a one-page PDF with a cross-reference table', () => {
		expect(body.startsWith('%PDF-1.4')).toBe(true);
		expect(body).toContain('/Type /Catalog');
		expect(body).toContain('/Count 1');
		expect(body.trimEnd().endsWith('%%EOF')).toBe(true);
		// `startxref` must point at the `xref` keyword, or a reader rebuilds
		// the file and some refuse it outright.
		const startxref = Number(/startxref\n(\d+)/u.exec(body)?.[1]);
		expect(body.slice(startxref, startxref + 4)).toBe('xref');
	});

	it('carries the key, in the two rows the sheet prints', () => {
		for (const row of keyRows(KEY)) {
			expect(body).toContain(row);
		}
		expect(keyRows(KEY)).toHaveLength(2);
	});

	it('states what is lost without the key', () => {
		// The promise of screen 2, in print: the sheet a user finds in a
		// drawer must say why it mattered.
		expect(body).toContain(STRINGS.warning);
		for (const advice of STRINGS.advice) {
			expect(body).toContain(advice);
		}
		// The em dash is written as a hyphen: WinAnsi has no U+2014, and a
		// mojibake pair on a printed sheet is worse than a hyphen.
		expect(body).toContain('Account: @you:example.com - example.com');
	});

	it('escapes the characters a PDF string cannot carry raw', () => {
		const escaped = recoveryKeyPdf({
			groupedKey: KEY,
			strings: { ...STRINGS, intro: 'A (parenthesis) and a \\ backslash' }
		});
		expect(text(escaped)).toContain('A \\(parenthesis\\) and a \\\\ backslash');
	});

	it('carries French, and drops what WinAnsi cannot', () => {
		const french = recoveryKeyPdf({
			groupedKey: KEY,
			strings: { ...STRINGS, title: 'Clé de récupération — « sûre »' }
		});
		// The accents survive; the em dash becomes a hyphen rather than a
		// mojibake pair.
		expect(text(french)).toContain('Clé de récupération - « sûre »');
	});
});
