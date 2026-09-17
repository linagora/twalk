// Drawing a QR code in the browser, from the raw payload the bridge handed
// over.
//
// **The bridge renders no image.** bridgev2's QR step is
// `{"type": "qr", "data": "2@…"}` — a string the phone's camera has to read —
// and the Gateway passes it through untouched (`openapi.yaml`,
// `BridgeLoginStep.payload`). So the encoding happens here, which is also what
// keeps the payload out of every network hop it does not need: it arrives in
// one polled JSON document and becomes an SVG in the same tick. Nothing writes
// it anywhere.
//
// `qrcode-generator` is the encoder (MIT, no dependencies of its own). This
// module is the only file that imports it: the app asks for a module grid, not
// for a library's idea of an `<img>` tag.

import qrcode from 'qrcode-generator';

/**
 * Error correction level **L**.
 *
 * The right one, not the safe-looking one: WhatsApp's and Signal's own clients
 * encode their linking codes at L, the payload is long (≈150–250 characters,
 * which is already QR version 9 to 13 at L), and a higher level buys redundancy
 * against print damage that a screen does not suffer while pushing the version
 * — and therefore the module count — up. More modules on a phone screen is a
 * code a camera fails to read, which is the failure this screen exists to
 * avoid.
 */
const ERROR_CORRECTION = 'L';

/**
 * UTF-8 for the byte-mode payload.
 *
 * The library's default truncates each UTF-16 code unit to its low byte, which
 * is right for the ASCII payloads WhatsApp and Signal actually send and wrong
 * for anything else. Setting it once here costs nothing and removes a class of
 * silently-corrupt code.
 */
const encoder = new TextEncoder();
qrcode.stringToBytes = (value: string) => Array.from(encoder.encode(value));

/** A square grid of modules: `grid[row][column]`, `true` where the code is dark. */
export interface QrGrid {
	readonly size: number;
	readonly modules: readonly (readonly boolean[])[];
}

/**
 * Encodes a payload as a module grid.
 *
 * `typeNumber: 0` lets the library pick the smallest version the payload fits
 * in, so a short code draws as a coarse grid a camera reads from further away.
 *
 * Throws when the payload is longer than a version-40 code holds. A caller on a
 * login screen treats that as "this step is not a code we can draw" and says so
 * — a blank square with no explanation is the one outcome to avoid.
 */
export function encodeQr(data: string): QrGrid {
	const code = qrcode(0, ERROR_CORRECTION);
	code.addData(data, 'Byte');
	code.make();
	const size = code.getModuleCount();
	const modules: boolean[][] = [];
	for (let row = 0; row < size; row += 1) {
		const line: boolean[] = [];
		for (let column = 0; column < size; column += 1) {
			line.push(code.isDark(row, column));
		}
		modules.push(line);
	}
	return { size, modules };
}

/**
 * The grid as one SVG path's `d` attribute: one `M`/`h`/`v` square per dark
 * module, in a 1-unit-per-module coordinate system.
 *
 * One path rather than a few hundred `<rect>`s — a version-13 code is 69×69,
 * so roughly 2 400 dark modules, and 2 400 elements is a measurable layout cost
 * on a phone for something that is one shape.
 */
export function qrPath(grid: QrGrid): string {
	const parts: string[] = [];
	for (let row = 0; row < grid.size; row += 1) {
		const line = grid.modules[row];
		if (line === undefined) {
			continue;
		}
		let column = 0;
		while (column < grid.size) {
			if (line[column] !== true) {
				column += 1;
				continue;
			}
			// Runs of dark modules collapse into one rectangle, which is most
			// of a finder pattern and every timing line.
			let run = 1;
			while (column + run < grid.size && line[column + run] === true) {
				run += 1;
			}
			parts.push(`M${column} ${row}h${run}v1h-${run}z`);
			column += run;
		}
	}
	return parts.join('');
}
