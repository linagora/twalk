// The recovery key, as the user reads it and types it back.
//
// This is Matrix's own "cryptographic key representation"
// (https://spec.matrix.org/v1.11/appendices/#cryptographic-key-representation),
// implemented here rather than imported from matrix-js-sdk on purpose: this
// module is what screen 2 formats with, what the store-loss screen validates
// with, and what the Playwright journey decodes with — and none of those may
// pull the 7.8 MB WebAssembly crypto stack into the module graph. The SDK's own
// `encodeRecoveryKey`/`decodeRecoveryKey` do the same thing inside the crypto
// bootstrap; this file is the outside, and its unit test asserts the two agree
// on the same bytes by construction (prefix, parity, base58).
//
// The layout, byte for byte:
//
//   0x8B 0x01  | 32 key bytes | XOR of every byte before it
//
// base58-encoded (the Bitcoin alphabet), which is 48 characters for those 35
// bytes — exactly the 12 groups of 4 that wireframe screen 2 displays.
//
// Pure. No `crypto`, no `window`, no dependency.

/** The two bytes every Matrix recovery key starts with. */
export const RECOVERY_KEY_PREFIX = [0x8b, 0x01] as const;

/** The number of key bytes carried: a 256-bit secret storage key. */
export const RECOVERY_KEY_BYTES = 32;

/** How the wireframes group the key for reading: 12 groups of 4. */
export const RECOVERY_KEY_GROUP = 4;

const BASE58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

/** Why a string the user typed is not a recovery key. */
export type RecoveryKeyProblem =
	/** Nothing typed yet. */
	| 'empty'
	/** A character that is not in the base58 alphabet. */
	| 'not-base58'
	/** Decodes, but not to 35 bytes. Usually a group too few or too many. */
	| 'wrong-length'
	/** The first two bytes are not `0x8B 0x01`: some other kind of key. */
	| 'not-a-recovery-key'
	/** The parity byte disagrees: a typo, or two characters swapped. */
	| 'parity';

export type DecodedRecoveryKey =
	| { ok: true; bytes: Uint8Array<ArrayBuffer> }
	| { ok: false; problem: RecoveryKeyProblem };

/**
 * Formats a key for display: the 48 characters in groups of four, separated by
 * a single space. Idempotent, so it can be applied to a value that is already
 * grouped.
 */
export function groupRecoveryKey(key: string): string {
	const compact = stripWhitespace(key);
	const groups: string[] = [];
	for (let at = 0; at < compact.length; at += RECOVERY_KEY_GROUP) {
		groups.push(compact.slice(at, at + RECOVERY_KEY_GROUP));
	}
	return groups.join(' ');
}

/**
 * What the user typed, with the spacing they may or may not have reproduced
 * removed. Case is *not* normalised: base58 distinguishes `A` from `a`, and
 * silently upper-casing a key would turn a correct one into a wrong one.
 */
export function compactRecoveryKey(key: string): string {
	return stripWhitespace(key);
}

/**
 * Decodes a recovery key back to the 32 bytes of the secret storage key, or
 * says why it could not. Every refusal is one the user can act on, which is
 * the whole reason this is not a boolean.
 */
export function decodeRecoveryKey(key: string): DecodedRecoveryKey {
	const compact = compactRecoveryKey(key);
	if (compact.length === 0) {
		return { ok: false, problem: 'empty' };
	}
	const decoded = base58Decode(compact);
	if (decoded === null) {
		return { ok: false, problem: 'not-base58' };
	}
	if (decoded.length !== RECOVERY_KEY_PREFIX.length + RECOVERY_KEY_BYTES + 1) {
		return { ok: false, problem: 'wrong-length' };
	}
	if (decoded[0] !== RECOVERY_KEY_PREFIX[0] || decoded[1] !== RECOVERY_KEY_PREFIX[1]) {
		return { ok: false, problem: 'not-a-recovery-key' };
	}
	let parity = 0;
	for (let at = 0; at < decoded.length - 1; at += 1) {
		parity ^= decoded[at] ?? 0;
	}
	if (parity !== decoded[decoded.length - 1]) {
		return { ok: false, problem: 'parity' };
	}
	return {
		ok: true,
		bytes: decoded.slice(RECOVERY_KEY_PREFIX.length, decoded.length - 1)
	};
}

/** Whether the store-loss screen may enable its "unlock" button. */
export function isValidRecoveryKey(key: string): boolean {
	return decodeRecoveryKey(key).ok;
}

/**
 * The inverse, for tests and for the key the crypto stack hands us in raw
 * form: 32 bytes in, the 48 displayable characters out.
 */
export function encodeRecoveryKey(bytes: Uint8Array): string {
	if (bytes.length !== RECOVERY_KEY_BYTES) {
		throw new Error(`a secret storage key is ${RECOVERY_KEY_BYTES} bytes, not ${bytes.length}`);
	}
	const full = new Uint8Array(RECOVERY_KEY_PREFIX.length + RECOVERY_KEY_BYTES + 1);
	full[0] = RECOVERY_KEY_PREFIX[0];
	full[1] = RECOVERY_KEY_PREFIX[1];
	full.set(bytes, RECOVERY_KEY_PREFIX.length);
	let parity = 0;
	for (let at = 0; at < full.length - 1; at += 1) {
		parity ^= full[at] ?? 0;
	}
	full[full.length - 1] = parity;
	return base58Encode(full);
}

function stripWhitespace(value: string): string {
	return value.replace(/\s+/gu, '');
}

/**
 * base58, the Bitcoin alphabet — big-endian, with leading zero bytes carried
 * as leading `1`s, which is what the Matrix appendix specifies.
 */
function base58Encode(bytes: Uint8Array): string {
	const digits: number[] = [0];
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
	let out = '';
	for (const byte of bytes) {
		if (byte !== 0) {
			break;
		}
		out += BASE58_ALPHABET[0];
	}
	for (let at = digits.length - 1; at >= 0; at -= 1) {
		out += BASE58_ALPHABET[digits[at] ?? 0];
	}
	return out;
}

/** `null` when the string holds a character the alphabet does not. */
function base58Decode(value: string): Uint8Array<ArrayBuffer> | null {
	const bytes: number[] = [0];
	for (const character of value) {
		const digit = BASE58_ALPHABET.indexOf(character);
		if (digit === -1) {
			return null;
		}
		let carry = digit;
		for (let at = 0; at < bytes.length; at += 1) {
			carry += (bytes[at] ?? 0) * 58;
			bytes[at] = carry & 0xff;
			carry >>= 8;
		}
		while (carry > 0) {
			bytes.push(carry & 0xff);
			carry >>= 8;
		}
	}
	let leadingZeroes = 0;
	for (const character of value) {
		if (character !== BASE58_ALPHABET[0]) {
			break;
		}
		leadingZeroes += 1;
	}
	const out = new Uint8Array(leadingZeroes + bytes.length);
	for (let at = 0; at < bytes.length; at += 1) {
		out[leadingZeroes + at] = bytes[bytes.length - 1 - at] ?? 0;
	}
	return out;
}
