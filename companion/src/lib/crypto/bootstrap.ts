// The cryptographic bootstrap: everything ADR 0014 says happens in the
// browser, and nowhere else.
//
// **Every import of matrix-js-sdk in the app goes through this module, and it
// imports the SDK dynamically.** That is what keeps 1.3 MB of brotli-
// compressed WebAssembly (7.8 MB raw, 4.8 MB with its `name` section stripped
// — see `vite.config.ts`) off screen 1, the diagnostics page and the capability
// gate. matrix-js-sdk loads the WebAssembly itself, lazily, inside
// `initRustCrypto`.
//
// **The order is load-bearing**, and it is the reason this is a module rather
// than a handful of calls in a component:
//
//   1. `initRustCrypto()` — brings up the Rust crypto machine over IndexedDB.
//   2. `bootstrapCrossSigning({ authUploadDeviceSigningKeys })` — creates the
//      cross-signing identity and publishes its public half.
//   3. `createRecoveryKeyFromPassphrase()` — **with no argument**. The
//      passphrase path is 500 000 PBKDF2 iterations, which is seconds of
//      frozen phone for a secret weaker than 32 random bytes, and the
//      wireframes offer no passphrase.
//   4. `bootstrapSecretStorage({ createSecretStorageKey, setupNewKeyBackup })`
//      — puts the cross-signing secrets *and* a fresh key backup behind that
//      key.
//
// Reverse 2 and 4 and the thing still succeeds: secret storage is created, the
// backup exists, and the cross-signing private keys are simply not in it. The
// user then has a recovery key that recovers nothing — which is the failure
// this ordering exists to prevent, and which no amount of UI can detect later.
//
// **What leaves this module.** `bootstrapSecretStorage` uploads the key
// *description* (its algorithm, its salt-free AES-HMAC parameters and a MAC
// used to check a key against it) to the user's own homeserver account data.
// The private key goes to `createSecretStorageKey`'s return value — this
// module's own callback — and from there to the screen, the clipboard and the
// PDF. It is never a request parameter, and `tests/e2e/bootstrap.spec.ts`
// asserts that against every request the page makes.

import type { MatrixClient } from 'matrix-js-sdk';
import type { GeneratedSecretStorageKey } from 'matrix-js-sdk/lib/crypto-api';

import { groupRecoveryKey } from '$lib/recovery/key';

/** The Matrix session the bootstrap runs in. */
export interface MatrixSession {
	/** The homeserver's base URL, as `$lib/matrix/discovery.ts` resolved it. */
	baseUrl: string;
	userId: string;
	deviceId: string;
	accessToken: string;
}

/** The steps, in order, for the progress the wireframe asks for. */
export type BootstrapStep =
	| 'loading-crypto'
	| 'cross-signing'
	| 'recovery-key'
	| 'secret-storage'
	| 'done';

export interface BootstrapResult {
	/** The 48 characters, in 12 groups of four: what screen 2 shows. */
	recoveryKey: string;
	userId: string;
	deviceId: string;
	/** Whether a key backup was created and is now the active version. */
	keyBackup: boolean;
}

/**
 * One client per tab, for as long as the tab lives. The SDK is blunt about
 * why: "the cryptography stack is not thread-safe. Having multiple
 * `MatrixClient` instances connected to the same Indexed DB will cause data
 * corruption and decryption failures." A second tab is kept out by the Web
 * Lock (`$lib/tabs/lock.ts`); a second *instance* is kept out here.
 */
let current: { client: MatrixClient; session: MatrixSession } | null = null;

/** The live client, for a caller that already ran a bootstrap. */
export function currentClient(): MatrixClient | null {
	return current?.client ?? null;
}

/**
 * Generates the account's cryptographic identity and its recovery key.
 *
 * `password` is the one the user just chose: uploading cross-signing keys is a
 * user-interactive-auth operation, and a password is the stage the homeserver
 * offers. It is used for that single request and is not kept.
 */
export async function bootstrapIdentity(
	session: MatrixSession,
	password: string,
	onStep: (step: BootstrapStep) => void = () => {}
): Promise<BootstrapResult> {
	onStep('loading-crypto');
	const client = await startClient(session);
	const crypto = client.getCrypto();
	if (crypto === undefined) {
		throw new Error('the crypto stack did not start');
	}

	onStep('cross-signing');
	await crypto.bootstrapCrossSigning({
		authUploadDeviceSigningKeys: async (makeRequest) => {
			await makeRequest({
				type: 'm.login.password',
				identifier: { type: 'm.id.user', user: session.userId },
				password
			});
		}
	});

	onStep('recovery-key');
	// No argument. See the module docs: the passphrase path is the one the
	// wireframes and ADR 0014 both refuse.
	const generated = await crypto.createRecoveryKeyFromPassphrase();
	const encoded = generated.encodedPrivateKey;
	if (encoded === undefined || encoded.length === 0) {
		throw new Error('the crypto stack generated no displayable recovery key');
	}

	onStep('secret-storage');
	await crypto.bootstrapSecretStorage({
		// The only place the private key travels to is this function's caller.
		createSecretStorageKey: async (): Promise<GeneratedSecretStorageKey> => generated,
		setupNewKeyBackup: true
	});

	const keyBackup = (await crypto.getActiveSessionBackupVersion()) !== null;
	onStep('done');

	return {
		recoveryKey: groupRecoveryKey(encoded),
		userId: session.userId,
		deviceId: session.deviceId,
		keyBackup
	};
}

/** Why the store-loss journey could not finish. */
export type RestoreProblem =
	/** The homeserver refused the password. */
	| 'wrong-password'
	/** The recovery key does not match the one in the account's secret storage. */
	| 'wrong-recovery-key'
	/** The account has no secret storage: there is nothing to recover from. */
	| 'no-secret-storage'
	/** Anything else, with the message in `detail`. */
	| 'failed';

export interface RestoreResult {
	userId: string;
	deviceId: string;
	accessToken: string;
	/** Whether this device now carries a cross-signed identity again. */
	verified: boolean;
	/** Whether the cross-signing identity is back in this device's store. */
	crossSigningReady: boolean;
	/** Whether this device's signature by that identity was published. */
	deviceSigned: boolean;
	/** Whether the key backup was found and its decryption key loaded. */
	keyBackup: boolean;
}

export class RestoreError extends Error {
	constructor(
		readonly problem: RestoreProblem,
		readonly detail: string
	) {
		super(`${problem}: ${detail}`);
		this.name = 'RestoreError';
	}
}

/**
 * The store-loss journey's engine: log in again on this browser, then pull the
 * cross-signing secrets and the key-backup key back out of the account's
 * secret storage with the recovery key the user typed.
 *
 * Note the asymmetry with [`bootstrapIdentity`]: nothing is created here. The
 * account's cryptographic identity already exists and must not be replaced —
 * replacing it would break every other device and lose the key backup, which
 * is exactly what screen 2's copy warns about. This is a *new device* joining
 * an existing identity.
 */
export async function restoreFromRecoveryKey(options: {
	baseUrl: string;
	userId: string;
	password: string;
	/** The 32 bytes behind the key the user typed (`$lib/recovery/key.ts`). */
	privateKey: Uint8Array<ArrayBuffer>;
	deviceName?: string;
	onStep?: (step: 'signing-in' | 'loading-crypto' | 'unlocking' | 'done') => void;
}): Promise<RestoreResult> {
	const onStep = options.onStep ?? (() => {});
	onStep('signing-in');
	const login = await passwordLogin(options.baseUrl, options.userId, options.password, options.deviceName);

	onStep('loading-crypto');
	const client = await startClient(
		{
			baseUrl: options.baseUrl,
			userId: login.userId,
			deviceId: login.deviceId,
			accessToken: login.accessToken
		},
		options.privateKey
	);
	const crypto = client.getCrypto();
	if (crypto === undefined) {
		throw new RestoreError('failed', 'the crypto stack did not start');
	}

	onStep('unlocking');
	const defaultKeyId = await client.secretStorage.getDefaultKeyId();
	if (defaultKeyId === null) {
		throw new RestoreError(
			'no-secret-storage',
			'this account has no secret storage, so there is nothing a recovery key opens'
		);
	}
	const description = await client.secretStorage.getKey(defaultKeyId);
	if (description !== null && description[1].algorithm === 'm.secret_storage.v1.aes-hmac-sha2') {
		const matches = await client.secretStorage.checkKey(options.privateKey, description[1]);
		if (!matches) {
			throw new RestoreError(
				'wrong-recovery-key',
				'the key does not match this account’s secret storage'
			);
		}
	}

	// The account's **public** cross-signing keys have to be inside the crypto
	// machine before the private ones can be imported into it: the machine
	// checks the pair, and refuses — with a message about importing, not about
	// downloading — when only the private half arrives. A device that has just
	// logged in has never queried its own user, and the Companion runs no sync
	// loop, so the query is made here, explicitly.
	//
	// `userHasCrossSigningKeys` rather than `getUserDeviceInfo`: it is the one
	// that asks the *machine* to run `/keys/query` and therefore the one whose
	// answer lands in the machine's store. `getUserDeviceInfo` downloads the
	// same document over plain HTTP and hands it to the caller, which leaves
	// the machine exactly as ignorant as before.
	const identityExists = await crypto.userHasCrossSigningKeys(login.userId, true);
	if (!identityExists) {
		throw new RestoreError(
			'no-secret-storage',
			'this account has no cross-signing identity to recover'
		);
	}

	// No `setupNewCrossSigning`: with the secrets reachable in secret storage
	// this imports them and cross-signs *this* device, leaving the account's
	// identity — and every other device's trust in it — alone.
	try {
		await crypto.bootstrapCrossSigning({});
	} catch (cause) {
		throw new RestoreError('failed', cause instanceof Error ? cause.message : String(cause));
	}

	let keyBackup = false;
	try {
		await crypto.loadSessionBackupPrivateKeyFromSecretStorage();
		await crypto.checkKeyBackupAndEnable();
		keyBackup = (await crypto.getActiveSessionBackupVersion()) !== null;
	} catch {
		// A missing or broken backup does not undo the recovered identity: the
		// device is verified either way, and the screen says which parts came
		// back.
	}

	// Sign this device with the identity that has just come back, and read the
	// signature in again.
	//
	// `bootstrapCrossSigning` does sign the device on the import path, but the
	// machine's own copy of the device carries the signature only once it has
	// read it back from the homeserver — so without the query below, a device
	// that *is* verified reports that it is not. `crossSignDevice` is harmless
	// when the signature is already there and is what makes the two steps
	// independent of the SDK's internal ordering.
	// Sign this device with the identity that has just come back, and publish
	// the signature: that is what tells the user's other devices — and the
	// Sensor — that this browser is theirs again.
	//
	// `bootstrapCrossSigning` does this too on the import path; doing it here
	// as well is idempotent and keeps the step visible rather than implied.
	let deviceSigned = true;
	try {
		await crypto.crossSignDevice(login.deviceId);
	} catch {
		deviceSigned = false;
	}

	// What "verified" means here, precisely, because the obvious check is the
	// wrong one: `getDeviceVerificationStatus(...).crossSigningVerified` reads
	// the crypto machine's *stored* copy of this device, and that copy only
	// learns about the signature above when a `/keys/query` response is fed
	// back into the machine — which happens on a sync, and the Companion runs
	// no sync loop. So it reports `false` on a device that every other client
	// sees as signed. The two facts that are true here and now are the ones
	// reported: the account's cross-signing identity is back in this device's
	// store and trusted (`isCrossSigningReady`, `getUserVerificationStatus`),
	// and the signature was published. `tests/e2e/bootstrap.spec.ts` asserts
	// the third — that the homeserver carries the signature — by asking the
	// homeserver, which is where the truth lives.
	const crossSigningReady = await crypto.isCrossSigningReady();
	const identityTrusted = (await crypto.getUserVerificationStatus(login.userId)).isVerified();
	const verified = crossSigningReady && identityTrusted && deviceSigned;

	onStep('done');
	return {
		userId: login.userId,
		deviceId: login.deviceId,
		accessToken: login.accessToken,
		verified,
		crossSigningReady,
		deviceSigned,
		keyBackup
	};
}

/**
 * `POST /_matrix/client/v3/login`, by hand rather than through
 * `client.login()`: this runs *before* there is a client, and building one to
 * throw it away would mean two clients in a tab.
 */
async function passwordLogin(
	baseUrl: string,
	userId: string,
	password: string,
	deviceName?: string
): Promise<{ userId: string; deviceId: string; accessToken: string }> {
	let response: Response;
	try {
		response = await fetch(`${baseUrl}/_matrix/client/v3/login`, {
			method: 'POST',
			credentials: 'omit',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				type: 'm.login.password',
				identifier: { type: 'm.id.user', user: userId },
				password,
				initial_device_display_name: deviceName ?? 'Twalk Companion'
			})
		});
	} catch (cause) {
		throw new RestoreError('failed', cause instanceof Error ? cause.message : 'login failed');
	}
	const body: unknown = await response.json().catch(() => ({}));
	if (!response.ok) {
		const errcode = (body as { errcode?: string }).errcode;
		throw errcode === 'M_FORBIDDEN' || errcode === 'M_UNAUTHORIZED'
			? new RestoreError('wrong-password', 'the homeserver refused the password')
			: new RestoreError('failed', `the homeserver answered ${response.status}`);
	}
	const session = body as { user_id?: string; device_id?: string; access_token?: string };
	if (
		typeof session.user_id !== 'string' ||
		typeof session.device_id !== 'string' ||
		typeof session.access_token !== 'string'
	) {
		throw new RestoreError('failed', 'the login response was not a session');
	}
	return {
		userId: session.user_id,
		deviceId: session.device_id,
		accessToken: session.access_token
	};
}

/**
 * Builds the tab's one client and starts the Rust crypto stack on it.
 *
 * `useIndexedDB` is left at its default, which is on. ADR 0014 chose that
 * knowingly: with an in-memory store every visit is a brand-new device,
 * publishing new keys and needing verification again, which is worse than a
 * store the browser may one day evict — and losing it is a designed journey,
 * not an error.
 */
async function startClient(
	session: MatrixSession,
	secretStorageKey?: Uint8Array<ArrayBuffer>
): Promise<MatrixClient> {
	if (current !== null) {
		if (current.session.accessToken === session.accessToken) {
			return current.client;
		}
		// A different session in the same tab: the old client must let go of
		// the store before another opens it.
		current.client.stopClient();
		current = null;
	}

	// Dynamic, and the only dynamic import of the SDK in the app: this is the
	// line that decides which screens pay for the crypto stack.
	const { createClient } = await import('matrix-js-sdk');

	/**
	 * The secret storage key, cached for the run. `bootstrapSecretStorage`
	 * warns that it may reach for the key several times, and a callback that
	 * asks the user twice for the same 48 characters is a callback nobody
	 * finishes.
	 */
	let cached: [string, Uint8Array<ArrayBuffer>] | null = null;

	const client = createClient({
		baseUrl: session.baseUrl,
		userId: session.userId,
		deviceId: session.deviceId,
		accessToken: session.accessToken,
		cryptoCallbacks: {
			cacheSecretStorageKey: (keyId, _info, key) => {
				cached = [keyId, key];
			},
			getSecretStorageKey: async ({ keys }) => {
				if (cached !== null && keys[cached[0]] !== undefined) {
					return cached;
				}
				if (secretStorageKey === undefined) {
					return null;
				}
				const keyId = Object.keys(keys)[0];
				return keyId === undefined ? null : [keyId, secretStorageKey];
			}
		}
	});

	await client.initRustCrypto();
	current = { client, session };
	return client;
}

/**
 * Lets go of the client and the crypto store. Called when this tab hands the
 * Web Lock to another one: the store must have exactly one owner.
 */
export function releaseClient(): void {
	if (current !== null) {
		current.client.stopClient();
		current = null;
	}
}
