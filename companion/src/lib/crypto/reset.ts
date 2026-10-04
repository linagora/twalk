// Starting the account's cryptographic identity over, from a browser that has
// neither its recovery key nor its store.
//
// This is the other half of `renew.ts`. That one replaces a lost key from a
// browser whose store is still there; this one is for the browser where
// nothing is left — which is the case that actually happened on the reference
// deployment on 2026-10-04, and the one the recovery screen used to answer by
// sending the owner to another Matrix client against a homeserver published
// nowhere (#439).
//
// **What it does is a reset, and the screen says so.** matrix-js-sdk will mint
// a new cross-signing identity with the password alone —
// `bootstrapCrossSigning({ setupNewCrossSigning: true })` — and the Companion
// has had that code since onboarding. It was withheld here because the screen
// claimed a reset "would break every other device and lose the message
// history". Both halves are measurable, and neither held on that deployment:
// `e2e_cross_signing_signatures` held no row signing any of the account's
// devices, and the key backup held 0 room keys against 132 encrypted rooms.
// So the cost is not a thing to warn about in the abstract — it is a thing to
// **measure on this account** before asking. That is [`whatAResetCosts`].

import type {
	CryptoApi,
	GeneratedSecretStorageKey,
} from 'matrix-js-sdk/lib/crypto-api';

import { groupRecoveryKey } from '$lib/recovery/key';

/**
 * The calls this module needs, named so a test can stand in for them. The
 * real one is `MatrixClient.getCrypto()`.
 */
export type ResettableCrypto = Pick<
	CryptoApi,
	| 'getUserDeviceInfo'
	| 'getDeviceVerificationStatus'
	| 'getKeyBackupInfo'
	| 'bootstrapCrossSigning'
	| 'createRecoveryKeyFromPassphrase'
	| 'bootstrapSecretStorage'
	| 'crossSignDevice'
	| 'getActiveSessionBackupVersion'
>;

/** What a reset produced, for the screen that shows it once. */
export interface ResetResult {
	/** The new key, in 12 groups of four. Shown once and never again. */
	recoveryKey: string;
	/** Whether the account now has a backup this key opens. Reported, never hidden. */
	keyBackup: boolean;
}

/** What a reset would take away from **this** account, measured. */
export interface ResetCost {
	/** Devices the account has, this browser's included. */
	devices: number;
	/**
	 * Devices currently signed by the account's cross-signing identity, which
	 * a reset un-signs: they keep working and stop being *verified*, so they
	 * have to be verified again — from this browser, with the new key.
	 */
	signedDevices: number;
	/**
	 * Room keys in the account's key backup, or `null` when the account has no
	 * backup at all. These are the messages a reset leaves unreadable from a
	 * fresh store — and `0` is the honest and common answer, because this
	 * client never syncs history and so has never put a key there.
	 */
	roomKeys: number | null;
}

/**
 * Asks the homeserver what the account holds, so the screen can state the
 * cost of a reset instead of warning about one.
 *
 * `downloadUncached` is on: this device has just logged in and has never
 * queried its own user, and the Companion runs no sync loop, so without it
 * the answer is an empty map and the screen would report "nothing to lose" on
 * an account with ten verified devices. Getting *that* wrong is the one
 * failure this function must not have.
 */
export async function whatAResetCosts(
	crypto: ResettableCrypto,
	userId: string,
): Promise<ResetCost> {
	const devices = await crypto.getUserDeviceInfo([userId], true);
	const ids = [...(devices.get(userId)?.keys() ?? [])];
	let signedDevices = 0;
	for (const deviceId of ids) {
		const status = await crypto.getDeviceVerificationStatus(userId, deviceId);
		if (status?.crossSigningVerified === true) {
			signedDevices += 1;
		}
	}
	// A backup this client cannot open still has a count, and the count is
	// what the owner needs: it says whether a reset loses messages or nothing.
	const backup = await crypto.getKeyBackupInfo().catch(() => null);
	return {
		devices: ids.length,
		signedDevices,
		roomKeys: backup === null ? null : backup.count,
	};
}

/**
 * Runs one step of the reset and, if it throws, says **which step** it was.
 *
 * Four SDK calls in a row, each able to fail with a message about the crypto
 * stack's internals: `getSecretStorageKey callback returned falsey` says
 * nothing about whether the identity was created, the key generated or the
 * storage reset — and the difference decides what the owner should do next.
 * Measured: that exact message reached the screen during this ticket's own
 * first real-stack run, and finding which call produced it took reading the
 * SDK rather than the error.
 */
async function step<T>(what: string, run: () => Promise<T>): Promise<T> {
	try {
		return await run();
	} catch (cause) {
		const detail = cause instanceof Error ? cause.message : String(cause);
		throw new Error(`${what}: ${detail}`, { cause });
	}
}

/**
 * Mints a new cross-signing identity and a new recovery key for the account,
 * and signs this browser's device with it.
 *
 * Returns the new key, grouped the way it is shown and written down: the only
 * moment it exists outside the crypto stack, exactly as at onboarding.
 *
 * **The order is the opposite of onboarding's, and that is the whole of what
 * this function knows.** `bootstrapIdentity` does cross-signing first and
 * secret storage after, because secret storage is what carries the new
 * private keys into 4S. Here that order cannot work, and the reason is in the
 * SDK rather than in anything this code chooses:
 *
 * ```js
 * // CrossSigningIdentity.resetCrossSigning
 * if (!(await this.secretStorage.hasKey())) { … not set up, nothing to do … }
 * else { await this.exportCrossSigningKeysToStorage(); }   // ← writes to 4S
 * ```
 *
 * The account **has** 4S — onboarding set it up — so a reset writes the new
 * private keys into it, which needs the key that opens it: the key this
 * screen's owner has lost. Measured, on the first real-stack run of this
 * ticket's own journey: `getSecretStorageKey callback returned falsey`, from
 * that line, before a single key was published.
 *
 * So secret storage goes **first**: a new 4S key replaces the unopenable one
 * and is cached by the client's `cacheSecretStorageKey` callback, and the
 * identity reset that follows writes its new secrets with that. Nothing is
 * lost by the swap — the local store holds no cross-signing secrets at this
 * point (that is what a reset is for), so the step onboarding relies on has
 * nothing to carry anyway, and `resetCrossSigning` stores them itself.
 *
 * It is **not atomic**, and the SDK says so in its own comment: a failure
 * between the two leaves a new 4S key the owner has not been shown and the
 * old identity still in place. The remedy is the same screen again, which is
 * why the error names the step it failed at.
 *
 * `setupNewKeyBackup: true`, which is where this differs from
 * [`renewRecoveryKey`](./renew.ts) — and the difference is the premise, not a
 * preference. There, the old key still worked for whoever had it, so minting
 * a backup version would have discarded a reachable one. Here the old key is
 * **lost**: whatever the existing backup holds is already unopenable, and
 * leaving it in place would leave the account with a backup version nothing
 * can decrypt. A fresh one loses nothing and leaves the account working.
 */
export async function resetIdentity(options: {
	crypto: ResettableCrypto;
	userId: string;
	/** For the user-interactive auth the key upload requires. Used once. */
	password: string;
	/** This browser's device, to be signed by the identity just created. */
	deviceId: string;
	onStep?: (step: 'recovery-key' | 'secret-storage' | 'identity') => void;
}): Promise<ResetResult> {
	const { crypto, userId, password, deviceId } = options;
	const onStep = options.onStep ?? ((): void => {});

	onStep('recovery-key');
	// No argument, for the reason `bootstrap.ts` gives: the passphrase path is
	// 500 000 PBKDF2 iterations for a secret weaker than 32 random bytes.
	const generated = await step(
		'generating a recovery key',
		async () => await crypto.createRecoveryKeyFromPassphrase(),
	);
	const encoded = generated.encodedPrivateKey;
	if (encoded === undefined || encoded.length === 0) {
		throw new Error('the crypto stack generated no displayable recovery key');
	}

	onStep('secret-storage');
	await step('replacing the account’s secret storage', async () =>
		await crypto.bootstrapSecretStorage({
			createSecretStorageKey: async (): Promise<GeneratedSecretStorageKey> =>
				generated,
			setupNewSecretStorage: true,
			setupNewKeyBackup: true,
		}),
	);

	onStep('identity');
	await step('creating a new cross-signing identity', async () =>
		await crypto.bootstrapCrossSigning({
			setupNewCrossSigning: true,
			// Written inline so the SDK's own types give `makeRequest` its
			// shape, exactly as `bootstrapIdentity` does.
			authUploadDeviceSigningKeys: async (makeRequest) => {
				await makeRequest({
					type: 'm.login.password',
					identifier: { type: 'm.id.user', user: userId },
					password,
				});
			},
		}),
	);

	// And sign this device with the identity that has just been created, so
	// the browser the owner is sitting at is verified rather than being the
	// one device the new identity does not vouch for. `bootstrapCrossSigning`
	// signs it on some paths; doing it here is idempotent and keeps the step
	// visible, the way the restore path does.
	await crypto.crossSignDevice(deviceId).catch(() => {
		// A signature that did not publish leaves the identity reset and the
		// key valid; the screen reports what it can see rather than failing
		// the whole reset over the last step.
	});

	return {
		recoveryKey: groupRecoveryKey(encoded),
		keyBackup: (await crypto.getActiveSessionBackupVersion()) !== null,
	};
}
