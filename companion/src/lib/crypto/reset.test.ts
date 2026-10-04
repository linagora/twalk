import type { CryptoApi } from 'matrix-js-sdk/lib/crypto-api';
import { describe, expect, it, vi } from 'vitest';

import { resetIdentity, whatAResetCosts, type ResettableCrypto } from './reset';

/** The options each call takes, so a spy's recorded call keeps its type. */
type CrossSigningOptions = Parameters<CryptoApi['bootstrapCrossSigning']>[0];
type SecretStorageOptions = Parameters<CryptoApi['bootstrapSecretStorage']>[0];

/** 48 base58 characters, which is what the crypto stack hands back. */
const ENCODED = 'EsTa1bQd2eFg3hIj4kLm5nOp6qRs7tUv8wXy9zAb1cDe2fGh';

/**
 * A crypto stack that records what it was asked to do, and in which order.
 */
function fakeCrypto(
	options: {
		encodedPrivateKey?: string | undefined;
		devices?: string[];
		signed?: string[];
		backup?: { count: number } | null;
	} = {},
) {
	const order: string[] = [];
	const bootstrapCrossSigning = vi.fn(async (_options: CrossSigningOptions) => {
		order.push('bootstrapCrossSigning');
	});
	const bootstrapSecretStorage = vi.fn(
		async (_options: SecretStorageOptions) => {
			order.push('bootstrapSecretStorage');
		},
	);
	const crossSignDevice = vi.fn(async (_deviceId: string) => {
		order.push('crossSignDevice');
	});
	const devices = options.devices ?? ['THISBROWSER'];
	const signed = new Set(options.signed ?? []);
	const crypto = {
		getUserDeviceInfo: async (userIds: string[]) =>
			new Map([
				[
					userIds[0],
					new Map(devices.map((deviceId) => [deviceId, {} as never])),
				],
			]) as never,
		getDeviceVerificationStatus: async (_userId: string, deviceId: string) =>
			({ crossSigningVerified: signed.has(deviceId) }) as never,
		getKeyBackupInfo: async () =>
			(options.backup === undefined ? null : options.backup) as never,
		bootstrapCrossSigning,
		createRecoveryKeyFromPassphrase: async () => {
			order.push('createRecoveryKeyFromPassphrase');
			return {
				encodedPrivateKey:
					'encodedPrivateKey' in options ? options.encodedPrivateKey : ENCODED,
				privateKey: new Uint8Array(),
			} as never;
		},
		bootstrapSecretStorage,
		crossSignDevice,
		getActiveSessionBackupVersion: async () => '7',
	} satisfies ResettableCrypto;
	return {
		crypto,
		order,
		bootstrapCrossSigning,
		bootstrapSecretStorage,
		crossSignDevice,
	};
}

const reset = async (crypto: ResettableCrypto) =>
	resetIdentity({
		crypto,
		userId: '@michel:twalk.localhost',
		password: 'the owner typed this',
		deviceId: 'THISBROWSER',
	});

describe('what a reset costs this account', () => {
	/**
	 * The measurement the screen states instead of a generic warning. On the
	 * reference deployment it was: three cross-signing keys, **no** signature
	 * on any device, and 0 room keys in the backup — so the sentence that
	 * withheld this feature ("would break every other device and lose the
	 * message history") was false on the one account it was protecting.
	 */
	it('counts the devices a reset un-signs, and the room keys it leaves unreadable', async () => {
		const { crypto } = fakeCrypto({
			devices: ['THISBROWSER', 'PHONE', 'LAPTOP'],
			signed: ['PHONE'],
			backup: { count: 12 },
		});

		const cost = await whatAResetCosts(crypto, '@michel:twalk.localhost');

		expect(cost).toEqual({ devices: 3, signedDevices: 1, roomKeys: 12 });
	});

	it('says nothing is signed and nothing is backed up when that is the truth', async () => {
		const { crypto } = fakeCrypto({
			devices: ['THISBROWSER', 'PHONE'],
			signed: [],
			backup: null,
		});

		const cost = await whatAResetCosts(crypto, '@michel:twalk.localhost');

		expect(cost).toEqual({ devices: 2, signedDevices: 0, roomKeys: null });
	});

	/**
	 * `downloadUncached` is the whole correctness of the probe. This device has
	 * just logged in, has never queried its own user, and the Companion runs no
	 * sync loop: without it the device map is empty and the screen would tell an
	 * owner with ten verified devices that a reset costs nothing.
	 */
	it('asks the homeserver rather than the local store', async () => {
		const getUserDeviceInfo = vi.fn(
			async () => new Map([['@michel:twalk.localhost', new Map()]]) as never,
		);
		const { crypto } = fakeCrypto();

		await whatAResetCosts(
			{ ...crypto, getUserDeviceInfo },
			'@michel:twalk.localhost',
		);

		expect(getUserDeviceInfo).toHaveBeenCalledWith(
			['@michel:twalk.localhost'],
			true,
		);
	});
});

describe('resetting the identity', () => {
	/**
	 * The one that matters. Without `setupNewCrossSigning` the SDK finds an
	 * identity already there and returns having done nothing — every call
	 * succeeds, the owner is shown a key, and the account is unchanged, which
	 * is the same silent no-op `renew.test.ts` guards against one layer along.
	 */
	it('mints a new cross-signing identity rather than finding one', async () => {
		const { crypto, bootstrapCrossSigning } = fakeCrypto();

		await reset(crypto);

		expect(bootstrapCrossSigning.mock.calls[0][0]).toMatchObject({
			setupNewCrossSigning: true,
		});
	});

	it('resets secret storage and mints a backup the new key can open', async () => {
		const { crypto, bootstrapSecretStorage } = fakeCrypto();

		await reset(crypto);

		expect(bootstrapSecretStorage.mock.calls[0][0]).toMatchObject({
			setupNewSecretStorage: true,
			setupNewKeyBackup: true,
		});
	});

	/**
	 * **The opposite order to onboarding's**, and the reason is the SDK's:
	 * `resetCrossSigning` writes the new private keys into the account's
	 * existing secret storage before publishing them, which needs the key that
	 * opens it — the key this screen's owner has lost. Measured on the first
	 * real-stack run of this ticket's journey: `getSecretStorageKey callback
	 * returned falsey`, with the identity not yet reset.
	 *
	 * So the storage is replaced first, and the identity reset writes into the
	 * new one. This assertion is that reasoning, kept where it can fail.
	 */
	it('replaces secret storage before resetting the identity', async () => {
		const { crypto, order } = fakeCrypto();

		await reset(crypto);

		expect(order).toEqual([
			'createRecoveryKeyFromPassphrase',
			'bootstrapSecretStorage',
			'bootstrapCrossSigning',
			'crossSignDevice',
		]);
	});

	it('authenticates the key upload with the password, once', async () => {
		const { crypto, bootstrapCrossSigning } = fakeCrypto();
		await reset(crypto);
		const makeRequest = vi.fn().mockResolvedValue(undefined);

		const authenticate =
			bootstrapCrossSigning.mock.calls[0][0]?.authUploadDeviceSigningKeys;
		await authenticate?.(makeRequest);

		expect(makeRequest).toHaveBeenCalledOnce();
		expect(makeRequest.mock.calls[0][0]).toEqual({
			type: 'm.login.password',
			identifier: { type: 'm.id.user', user: '@michel:twalk.localhost' },
			password: 'the owner typed this',
		});
	});

	it('hands back the key grouped the way it is shown and written down', async () => {
		const { crypto } = fakeCrypto();

		const { recoveryKey, keyBackup } = await reset(crypto);

		expect(recoveryKey).toMatch(/^(\w{4} ){11}\w{4}$/);
		// Minted here, where `renewRecoveryKey` leaves the old one alone: the
		// key that opened the old backup is the one this screen's owner lost.
		expect(keyBackup).toBe(true);
	});

	/**
	 * A crypto stack that generates no key must not reach secret storage: the
	 * identity is already reset at that point, and resetting storage on top
	 * would leave the owner with an account whose secrets nothing opens.
	 */
	it('refuses before touching secret storage when no key was generated', async () => {
		const { crypto, bootstrapSecretStorage } = fakeCrypto({
			encodedPrivateKey: undefined,
		});

		await expect(reset(crypto)).rejects.toThrow('no displayable recovery key');
		expect(bootstrapSecretStorage).not.toHaveBeenCalled();
	});

	/**
	 * The signature is the last step and the least of them: the identity is
	 * reset and the key is valid without it, so a failure there is reported by
	 * the screen rather than thrown over the whole reset.
	 */
	it('survives a signature that does not publish', async () => {
		const { crypto } = fakeCrypto();
		const crossSignDevice = vi.fn().mockRejectedValue(new Error('no'));

		await expect(
			reset({ ...crypto, crossSignDevice }),
		).resolves.toMatchObject({ recoveryKey: expect.any(String) });
	});
});
