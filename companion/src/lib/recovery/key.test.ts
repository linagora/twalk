// The recovery key's encoding, which is the one piece of this journey where a
// silent mistake is unrecoverable: a key displayed wrong is a key that will not
// open the account, and the user finds out months later.

import { describe, expect, it } from 'vitest';

import {
	compactRecoveryKey,
	decodeRecoveryKey,
	encodeRecoveryKey,
	groupRecoveryKey,
	isValidRecoveryKey,
	RECOVERY_KEY_BYTES
} from './key';

/** 32 bytes, deterministic, so a failure names the same key every time. */
function sampleKey(seed = 1): Uint8Array {
	const bytes = new Uint8Array(RECOVERY_KEY_BYTES);
	for (let at = 0; at < bytes.length; at += 1) {
		bytes[at] = (at * 7 + seed * 31) & 0xff;
	}
	return bytes;
}

describe('the Matrix key representation', () => {
	it('is 48 characters, which is the wireframe’s 12 groups of 4', () => {
		const encoded = encodeRecoveryKey(sampleKey());
		expect(encoded).toHaveLength(48);
		expect(groupRecoveryKey(encoded).split(' ')).toHaveLength(12);
		expect(groupRecoveryKey(encoded)).toMatch(/^(\w{4} ){11}\w{4}$/);
	});

	it('round-trips every byte', () => {
		for (const seed of [0, 1, 2, 7, 255]) {
			const key = sampleKey(seed);
			const decoded = decodeRecoveryKey(encodeRecoveryKey(key));
			expect(decoded.ok).toBe(true);
			if (decoded.ok) {
				expect(Array.from(decoded.bytes)).toEqual(Array.from(key));
			}
		}
	});

	it('accepts the key however the user spaced it', () => {
		const encoded = encodeRecoveryKey(sampleKey());
		for (const spelling of [
			encoded,
			groupRecoveryKey(encoded),
			`  ${groupRecoveryKey(encoded)}  `,
			encoded.replace(/(.{8})/gu, '$1\n')
		]) {
			expect(isValidRecoveryKey(spelling), spelling).toBe(true);
		}
	});

	it('does not touch case, because base58 does not', () => {
		// Upper-casing a correct key would make it wrong, so the field must
		// never do it helpfully.
		const encoded = encodeRecoveryKey(sampleKey());
		expect(compactRecoveryKey(` ${encoded} `)).toBe(encoded);
	});
});

describe('decodeRecoveryKey refuses', () => {
	const good = encodeRecoveryKey(sampleKey());

	it('nothing typed', () => {
		expect(decodeRecoveryKey('   ')).toEqual({ ok: false, problem: 'empty' });
	});

	it('a character base58 does not have', () => {
		// The four base58 leaves out, which are exactly the four a human
		// confuses: 0/O and I/l.
		expect(decodeRecoveryKey(`0${good.slice(1)}`)).toEqual({
			ok: false,
			problem: 'not-base58'
		});
	});

	it('a key of the wrong length', () => {
		expect(decodeRecoveryKey(good.slice(0, 44)).ok).toBe(false);
		expect(decodeRecoveryKey(good.slice(0, 44))).toMatchObject({ problem: 'wrong-length' });
	});

	it('a key that is not a recovery key', () => {
		// 35 bytes with another prefix: the right shape, the wrong kind.
		const other = new Uint8Array(35);
		other[0] = 0xed;
		other[1] = 0x01;
		let parity = 0;
		for (let at = 0; at < 34; at += 1) {
			parity ^= other[at] ?? 0;
		}
		other[34] = parity;
		const encoded = base58(other);
		expect(decodeRecoveryKey(encoded)).toMatchObject({ problem: 'not-a-recovery-key' });
	});

	it('a typo, through the parity byte', () => {
		// Swapping two characters keeps the length and the alphabet, and is
		// the mistake a human typing 48 characters actually makes.
		const typo = `${good.slice(0, 10)}${good[11]}${good[10]}${good.slice(12)}`;
		expect(typo).not.toBe(good);
		expect(decodeRecoveryKey(typo).ok).toBe(false);
	});
});

/** A local base58 encoder, so the "wrong prefix" case does not use the code under test's own prefix. */
function base58(bytes: Uint8Array): string {
	const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
	const digits = [0];
	for (const byte of bytes) {
		let carry = byte;
		for (let at = 0; at < digits.length; at += 1) {
			carry += (digits[at] ?? 0) << 8;
			digits[at] = carry % 58;
			carry = (carry / 58) | 0;
		}
		while (carry > 0) {
			digits.push(carry % 58);
			carry = (carry / 58) | 0;
		}
	}
	return digits
		.reverse()
		.map((digit) => alphabet[digit])
		.join('');
}
