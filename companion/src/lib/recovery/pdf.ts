// The printable one-page PDF wireframe screen 2 asks for ("Save as PDF").
//
// Written by hand, in about a hundred lines, rather than pulled in from a PDF
// library. Three reasons, in order:
//
//   1. The document carries the user's recovery key. A library would put third
//      party code on the one code path ADR 0014 promises nothing escapes from,
//      and the promise is only as good as what we can read.
//   2. The Companion loads no third-party origin (spec #65) and the bootstrap
//      screens already pay for 1.3 MB of WebAssembly. A PDF writer that costs
//      nothing is worth more than one that formats nicely.
//   3. It is pure: bytes in, bytes out, no browser API. So Vitest can assert
//      the key is in the document and the structure is a valid PDF, without a
//      browser.
//
// The output uses the two base-14 fonts every reader has (Helvetica and
// Courier-Bold), so no font is embedded, and leaves the content stream
// uncompressed — which is also what lets a test grep the bytes for the key.
//
// Text is encoded WinAnsi (Latin-1), which covers French. Anything outside it
// is dropped rather than mojibaked: the load-bearing content is the key, and
// the key is base58.

/** The strings the document shows, so the caller translates them, not this. */
export interface RecoveryKeyDocumentStrings {
	/** Document title, at the top. */
	title: string;
	/** One or two lines under the title saying what this is. */
	intro: string;
	/** The label above the key itself. */
	keyLabel: string;
	/** What is lost without it. Screen 2's promise, in print. */
	warning: string;
	/** Numbered advice on where to keep it. One string per line. */
	advice: string[];
	/** The line naming the account and the deployment. */
	account: string;
	/** The line naming when the document was made. */
	generated: string;
}

export interface RecoveryKeyDocument {
	/** The key, as it is displayed: 12 groups of 4. */
	groupedKey: string;
	strings: RecoveryKeyDocumentStrings;
}

// A4 in PostScript points, which is the unit every PDF coordinate is in.
const PAGE_WIDTH = 595.28;
const PAGE_HEIGHT = 841.89;
const MARGIN = 56;

/**
 * Builds the document. Returns the bytes; the caller turns them into a
 * download (`$lib/recovery/download.ts`).
 */
export function recoveryKeyPdf(document: RecoveryKeyDocument): Uint8Array {
	const { strings } = document;
	const lines: TextLine[] = [];
	let y = PAGE_HEIGHT - MARGIN;

	lines.push({ text: strings.title, font: 'bold', size: 20, y });
	y -= 34;
	for (const line of wrap(strings.intro, 78)) {
		lines.push({ text: line, font: 'regular', size: 11, y });
		y -= 16;
	}

	y -= 18;
	lines.push({ text: strings.keyLabel, font: 'bold', size: 11, y });
	y -= 28;
	// Two rows of six groups: 24 characters plus 5 spaces at 15 pt Courier is
	// 261 pt, which fits the margins on A4 with room to spare.
	for (const row of keyRows(document.groupedKey)) {
		lines.push({ text: row, font: 'mono', size: 15, y });
		y -= 24;
	}

	y -= 16;
	for (const line of wrap(strings.warning, 78)) {
		lines.push({ text: line, font: 'bold', size: 11, y });
		y -= 16;
	}

	y -= 14;
	for (const advice of strings.advice) {
		for (const line of wrap(advice, 80)) {
			lines.push({ text: line, font: 'regular', size: 11, y });
			y -= 16;
		}
	}

	y -= 20;
	for (const line of [strings.account, strings.generated]) {
		lines.push({ text: line, font: 'regular', size: 9, y });
		y -= 13;
	}

	return assemble(contentStream(lines));
}

/** Six groups per row, so the key reads as two lines rather than a paragraph. */
export function keyRows(groupedKey: string): string[] {
	const groups = groupedKey.trim().split(/\s+/u).filter((group) => group.length > 0);
	const rows: string[] = [];
	for (let at = 0; at < groups.length; at += 6) {
		rows.push(groups.slice(at, at + 6).join(' '));
	}
	return rows;
}

interface TextLine {
	text: string;
	font: 'regular' | 'bold' | 'mono';
	size: number;
	y: number;
}

const FONT_RESOURCE = { regular: '/F1', bold: '/F2', mono: '/F3' } as const;

function contentStream(lines: TextLine[]): string {
	const out: string[] = ['BT'];
	for (const line of lines) {
		out.push(`${FONT_RESOURCE[line.font]} ${line.size} Tf`);
		out.push(`1 0 0 1 ${MARGIN.toFixed(2)} ${line.y.toFixed(2)} Tm`);
		out.push(`(${escapeText(line.text)}) Tj`);
	}
	out.push('ET');
	return out.join('\n');
}

/**
 * Greedy wrapping at a character count rather than a measured width. The
 * document is one page of prose around a fixed-width key; a text measurement
 * table for Helvetica would be more code than the whole writer.
 */
function wrap(text: string, columns: number): string[] {
	const words = text.split(/\s+/u).filter((word) => word.length > 0);
	const lines: string[] = [];
	let current = '';
	for (const word of words) {
		const candidate = current === '' ? word : `${current} ${word}`;
		if (candidate.length > columns && current !== '') {
			lines.push(current);
			current = word;
		} else {
			current = candidate;
		}
	}
	if (current !== '') {
		lines.push(current);
	}
	return lines.length === 0 ? [''] : lines;
}

/** PDF literal strings escape their delimiters and the backslash. */
function escapeText(text: string): string {
	return toWinAnsi(text).replace(/([\\()])/gu, '\\$1');
}

/** Drops anything WinAnsi cannot carry, rather than emitting broken bytes. */
function toWinAnsi(text: string): string {
	let out = '';
	for (const character of text) {
		const code = character.codePointAt(0) ?? 0;
		if (code === 0x2019 || code === 0x2018) {
			out += "'";
		} else if (code === 0x201c || code === 0x201d) {
			out += '"';
		} else if (code === 0x2014 || code === 0x2013) {
			out += '-';
		} else if (code === 0x2026) {
			out += '...';
		} else if (code >= 0x20 && code <= 0xff) {
			out += character;
		}
	}
	return out;
}

/**
 * The file itself: six objects, a cross-reference table with the byte offset
 * of each, and a trailer. Byte offsets are counted in Latin-1 code units,
 * which is also how the bytes are written out — so the table cannot disagree
 * with the file.
 */
function assemble(stream: string): Uint8Array {
	const objects = [
		'<< /Type /Catalog /Pages 2 0 R >>',
		'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
		`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${PAGE_WIDTH.toFixed(2)} ${PAGE_HEIGHT.toFixed(
			2
		)}] /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R >> >> /Contents 4 0 R >>`,
		`<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
		'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>',
		'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>',
		'<< /Type /Font /Subtype /Type1 /BaseFont /Courier-Bold /Encoding /WinAnsiEncoding >>'
	];

	let file = '%PDF-1.4\n';
	const offsets: number[] = [];
	objects.forEach((body, index) => {
		offsets.push(file.length);
		file += `${index + 1} 0 obj\n${body}\nendobj\n`;
	});

	const startxref = file.length;
	file += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
	for (const offset of offsets) {
		file += `${String(offset).padStart(10, '0')} 00000 n \n`;
	}
	file += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${startxref}\n%%EOF\n`;

	const bytes = new Uint8Array(file.length);
	for (let at = 0; at < file.length; at += 1) {
		bytes[at] = file.charCodeAt(at) & 0xff;
	}
	return bytes;
}
