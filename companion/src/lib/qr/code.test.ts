// The QR encoder. A code drawn wrong is a code a phone camera silently fails
// to read, so these check the properties a scanner depends on rather than the
// exact bytes of one payload.

import { describe, expect, it } from 'vitest';

import { encodeQr, qrPath } from './code';

/** Reads the three finder patterns back out of the grid. */
function isFinderAt(grid: ReturnType<typeof encodeQr>, row: number, column: number): boolean {
	// The 7×7 finder: a dark ring, a light ring, a 3×3 dark core.
	for (let y = 0; y < 7; y += 1) {
		for (let x = 0; x < 7; x += 1) {
			const onRing = y === 0 || y === 6 || x === 0 || x === 6;
			const inCore = y >= 2 && y <= 4 && x >= 2 && x <= 4;
			const expected = onRing || inCore;
			if (grid.modules[row + y]?.[column + x] !== expected) {
				return false;
			}
		}
	}
	return true;
}

describe('the QR encoder', () => {
	it('encodes a WhatsApp-shaped payload into a valid grid', () => {
		// The shape of a real one: four comma-separated base64 fields.
		const payload =
			'2@Fk3Ux9nVQdE1s6Yl0aQ2c1KxT8zPqRfVw7BnJmHgLkOiUyTrEwQaZxCvBnMlKjHgFdSaPoIuYtRe,' +
			'kS9LpQz0XcVbNmAsDfGhJkLqWeRtYuIoPaSdFgHj=,' +
			'BvCxZaSdQwErTyUiOpAsDfGhJkLzXcVbNmQwErTy=,1';
		const grid = encodeQr(payload);

		// Square, odd-sized, and one of the QR versions (21 + 4n).
		expect(grid.size).toBe(grid.modules.length);
		expect((grid.size - 21) % 4).toBe(0);

		// The three finder patterns a camera locks on to.
		expect(isFinderAt(grid, 0, 0)).toBe(true);
		expect(isFinderAt(grid, 0, grid.size - 7)).toBe(true);
		expect(isFinderAt(grid, grid.size - 7, 0)).toBe(true);
	});

	it('picks the smallest version a payload fits in', () => {
		// A short code is a coarse grid, which a camera reads from further away.
		const small = encodeQr('2@short');
		const large = encodeQr('2@'.padEnd(400, 'x'));
		expect(small.size).toBeLessThan(large.size);
	});

	it('encodes a payload outside Latin-1 rather than corrupting it', () => {
		// The library's own default truncates to the low byte. Two payloads
		// that differ only above U+00FF must not encode identically.
		const one = qrPath(encodeQr('paiement — café ✓'));
		const other = qrPath(encodeQr('paiement — café ✔'));
		expect(one).not.toBe(other);
	});

	it('refuses a payload no QR version holds', () => {
		expect(() => encodeQr('x'.padEnd(5000, 'x'))).toThrow();
	});
});

describe('the drawn path', () => {
	it('collapses a run of dark modules into one rectangle', () => {
		const grid = encodeQr('2@a-payload-for-the-path');
		const path = qrPath(grid);
		// One sub-path per run, each a closed rectangle.
		expect(path.startsWith('M')).toBe(true);
		expect(path.match(/M/g)?.length).toBeGreaterThan(0);
		// A finder pattern's top edge is a run of 7, so at least one `h7` is
		// there — proof the runs are merged rather than drawn module by module.
		expect(path).toContain('h7');
		// Never more sub-paths than there are dark modules.
		const dark = grid.modules.flat().filter(Boolean).length;
		expect(path.match(/M/g)!.length).toBeLessThan(dark);
	});

	it('draws nothing for a grid with no dark modules', () => {
		expect(qrPath({ size: 3, modules: [[false, false, false]] })).toBe('');
	});
});
