// The bootstrap journey of ticket #67, end to end, against a real Companion
// Gateway and a real Synapse: domain, account, cross-signing, recovery key —
// then the store-loss journey that puts it all back.
//
// One file, `serial`, one account and **one browser context**, deliberately.
// The Gateway creates this deployment's one account and refuses a second,
// which is the rule the journey is built on rather than an obstacle to it; and
// the things that make a returning user a returning user — the `HttpOnly`
// device cookie and the crypto store in IndexedDB — live in the context, so a
// fresh context per test would be a fresh device per test and the journey
// would not exist.
//
// The assertions that matter most are not the happy path — they are:
//
//   - **no request carries the private key**, in any URL, header or body, over
//     the whole journey. That is screen 2's promise ("Twalk never sees it") and
//     ADR 0014's reason for existing, and it is asserted rather than assumed.
//   - the key **decodes back to 32 bytes**, so what is displayed is a key and
//     not a rendering of one.
//   - screen 2's copy says **what is lost** without it, and that there is no
//     reset.
//   - a **reload stays signed in**, which is the device cookie plus the crypto
//     store, and nothing this app wrote to browser storage.

import { readFile } from 'node:fs/promises';

import { expect, test, type BrowserContext, type Page, type Request } from '@playwright/test';

import { decodeRecoveryKey } from '../../src/lib/recovery/key';
import { CRYPTO_STORE_NAME } from '../../src/lib/crypto/store';
import { NO_STACK, realStack } from './stack';

const stack = realStack();

// The crypto bootstrap is WebAssembly, cross-signing, a secret-storage write
// and a key-backup creation against a real homeserver.
test.setTimeout(240_000);

test.describe.serial('the bootstrap journey', () => {
	test.skip(stack === null, NO_STACK);

	const password = 'correct-horse-battery-9';
	let recoveryKey = '';
	let context: BrowserContext;
	let page: Page;
	/** Every request the page has made since the last `forget`. */
	let requests: Request[] = [];

	test.beforeAll(async ({ browser }) => {
		context = await browser.newContext({
			// The wireframes are mobile-first, 375–428 CSS px; the journey is
			// asserted at the narrow end, where it has to work.
			viewport: { width: 390, height: 844 }
		});
		page = await context.newPage();
		page.on('request', (request) => requests.push(request));
	});

	test.afterAll(async () => {
		await context?.close();
	});

	function forget(): void {
		requests = [];
	}

	/**
	 * The strongest available form of "the Gateway never sees it": the 48
	 * displayed characters, their compact spelling, and the 32 bytes behind
	 * them in hex and base64, looked for in every URL, header and body of
	 * every request the page made.
	 */
	async function assertKeyNeverLeft(key: string): Promise<void> {
		const decoded = decodeRecoveryKey(key);
		expect(decoded.ok, 'the displayed key decodes').toBe(true);
		const needles = [key, key.replace(/\s+/gu, '')];
		if (decoded.ok) {
			needles.push(
				Array.from(decoded.bytes)
					.map((byte) => byte.toString(16).padStart(2, '0'))
					.join(''),
				btoa(String.fromCharCode(...decoded.bytes)).replace(/=+$/u, '')
			);
		}

		for (const request of requests) {
			const haystacks = [request.url(), JSON.stringify(request.headers())];
			const body = request.postData();
			if (body !== null) {
				haystacks.push(body);
			}
			for (const haystack of haystacks) {
				for (const needle of needles) {
					expect(
						haystack.includes(needle),
						`${needle.slice(0, 12)}… appeared in ${request.method()} ${request.url()}`
					).toBe(false);
				}
			}
		}
		// A run that recorded nothing would also pass the loop above.
		expect(requests.length, 'requests were recorded').toBeGreaterThan(5);
	}

	test('screen 1 probes the deployment and hands the domain on', async () => {
		if (stack === null) {
			return;
		}
		forget();
		await page.goto('/');
		await expect(page.getByTestId('screen-welcome')).toBeVisible();

		await page.getByLabel('Your Twalk domain').fill(stack.domain);
		await page.getByTestId('continue').click();

		// The probe reached a real Gateway (`GET /api/session` → 401, the API
		// is open) and a real homeserver (`/_matrix/client/versions`), so this
		// is screen 2 rather than the wireframe's error banner.
		await expect(page.getByTestId('screen-onboarding')).toBeVisible();
		await expect(page.getByTestId('chosen-domain')).toContainText(stack.domain);
	});

	test('screen 2 creates the account and shows the recovery key once', async () => {
		if (stack === null) {
			return;
		}
		await page.getByLabel('Username', { exact: true }).fill(stack.owner);
		await page.getByLabel('Password', { exact: true }).fill(password);
		await page.getByLabel('Confirm password').fill(password);
		await page.getByTestId('create-account').click();

		await expect(page.getByTestId('screen-recovery-key')).toBeVisible({ timeout: 180_000 });

		// Read row by row: the card prints the key as two lines of six groups,
		// and `textContent` on the block would run the two lines together.
		const rows = await page.getByTestId('recovery-key-row').allTextContents();
		expect(rows).toHaveLength(2);
		recoveryKey = rows.join(' ').replace(/\s+/gu, ' ').trim();

		// The wireframe's shape, which is the Matrix key representation for
		// 32 bytes plus its two-byte prefix and its parity byte.
		expect(recoveryKey.split(' ')).toHaveLength(12);
		expect(recoveryKey.replace(/\s/gu, '')).toHaveLength(48);
		const decoded = decodeRecoveryKey(recoveryKey);
		expect(decoded.ok).toBe(true);
		if (decoded.ok) {
			expect(decoded.bytes).toHaveLength(32);
		}

		// The key backup exists, which is what `setupNewKeyBackup` in the
		// bootstrap ordering is for — and what the ordering would silently
		// lose if secret storage came before cross-signing.
		await expect(page.getByTestId('key-backup-state')).toContainText(/key backup is on/i);
	});

	test('the copy says what is lost without the key, and that there is no reset', async () => {
		if (stack === null) {
			return;
		}
		const warning = page.getByTestId('recovery-key-warning');
		await expect(warning).toContainText(/encrypted for ever/i);
		await expect(warning).toContainText(/no longer be verified/i);
		await expect(warning).toContainText(/no reset in this version/i);
	});

	test('the key saves as a printable PDF, built in the browser', async () => {
		if (stack === null) {
			return;
		}
		const [download] = await Promise.all([
			page.waitForEvent('download'),
			page.getByTestId('save-recovery-pdf').click()
		]);
		expect(download.suggestedFilename()).toMatch(/^twalk-recovery-key-.*\.pdf$/u);
		const saved = await download.path();
		const pdf = await readFile(saved, 'latin1');
		expect(pdf.startsWith('%PDF-')).toBe(true);
		// The first of the sheet's two rows of six groups.
		expect(pdf).toContain(recoveryKey.split(' ').slice(0, 6).join(' '));
	});

	test('the checkbox gates continuing, and no request carried the key', async () => {
		if (stack === null) {
			return;
		}
		await expect(page.getByTestId('recovery-key-continue')).toBeDisabled();
		await page.getByTestId('saved-checkbox').check();
		await expect(page.getByTestId('recovery-key-continue')).toBeEnabled();
		await page.getByTestId('recovery-key-continue').click();

		await expect(page.getByTestId('onboarding-done')).toBeVisible();
		await expect(page.getByTestId('account-id')).toContainText(
			`@${stack.owner}:${stack.serverName}`
		);

		// Everything from the first `goto` of the journey to here.
		await assertKeyNeverLeft(recoveryKey);
	});

	test('a reload stays signed in', async () => {
		if (stack === null) {
			return;
		}
		// The device token is an `HttpOnly` cookie and the crypto store is in
		// IndexedDB; this app writes neither. "Still signed in" is therefore a
		// fact about the Gateway and the browser, not about a flag we set.
		await page.goto('/');
		await expect(page.getByTestId('already-signed-in')).toBeVisible();
		await expect(page.getByTestId('already-signed-in')).toContainText(stack.ownerId);

		const stored = await page.evaluate(
			async (name) => (await indexedDB.databases()).some((db) => db.name === name),
			CRYPTO_STORE_NAME
		);
		expect(stored, 'the crypto store persisted').toBe(true);
	});

	/**
	 * Logs in to Synapse from the test process and reads `/keys/query` for the
	 * owner: every live device must carry a signature by the account's
	 * self-signing key.
	 */
	async function expectDeviceIsCrossSigned(
		where: NonNullable<ReturnType<typeof realStack>>,
		accountPassword: string
	): Promise<void> {
		const login = await fetch(`${where.synapseUrl}/_matrix/client/v3/login`, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				type: 'm.login.password',
				identifier: { type: 'm.id.user', user: where.ownerId },
				password: accountPassword
			})
		});
		const session = (await login.json()) as { access_token: string };

		const query = await fetch(`${where.synapseUrl}/_matrix/client/v3/keys/query`, {
			method: 'POST',
			headers: {
				'content-type': 'application/json',
				authorization: `Bearer ${session.access_token}`
			},
			body: JSON.stringify({ device_keys: { [where.ownerId]: [] } })
		});
		const keys = (await query.json()) as {
			device_keys?: Record<string, Record<string, { signatures?: Record<string, object> }>>;
			self_signing_keys?: Record<string, { keys: Record<string, string> }>;
		};

		const selfSigning = Object.values(keys.self_signing_keys?.[where.ownerId]?.keys ?? {})[0];
		expect(selfSigning, 'the account publishes a self-signing key').toBeDefined();

		const devices = keys.device_keys?.[where.ownerId] ?? {};
		expect(Object.keys(devices).length).toBeGreaterThan(0);
		const signed = Object.values(devices).filter((device) =>
			Object.keys(device.signatures?.[where.ownerId] ?? {}).includes(`ed25519:${selfSigning}`)
		);
		expect(signed.length, 'a device is cross-signed on the homeserver').toBeGreaterThan(0);
	}

	test('a browser whose store was evicted is walked back in with the key', async () => {
		if (stack === null) {
			return;
		}
		// What iOS does after seven days without interaction, and what
		// "clear browsing data" does everywhere: the keys go, the Gateway
		// cookie may stay. ADR 0014 calls this normal, and this is the journey.
		await page.evaluate(
			async (name) =>
				await new Promise<void>((resolve) => {
					const request = indexedDB.deleteDatabase(name);
					request.onsuccess = () => resolve();
					request.onerror = () => resolve();
					request.onblocked = () => resolve();
				}),
			CRYPTO_STORE_NAME
		);

		forget();

		// Recognised on the next visit, without the user asking for anything.
		await page.goto('/');
		await expect(page.getByTestId('screen-recover')).toBeVisible();
		await expect(page.getByTestId('recover-account')).toContainText(stack.ownerId);

		// A key that is not a key cannot even be submitted.
		await page.getByLabel('Password', { exact: true }).fill(password);
		await page.getByTestId('recovery-key-input').fill('AAAA BBBB CCCC DDDD');
		await expect(page.getByTestId('recover-submit')).toBeDisabled();

		// The real one puts the device back: the cross-signing secrets come
		// out of secret storage and this new device is signed by them.
		await page.getByTestId('recovery-key-input').fill(recoveryKey);
		await expect(page.getByTestId('recover-submit')).toBeEnabled();
		await page.getByTestId('recover-submit').click();

		await expect(page.getByTestId('recover-done')).toBeVisible({ timeout: 180_000 });
		// Both halves of "verified", named separately so a regression says
		// which one broke: the identity is back in this device's store, and
		// this device's signature by it was published.
		await expect(page.getByTestId('recover-done')).toHaveAttribute(
			'data-cross-signing-ready',
			'true'
		);
		await expect(page.getByTestId('recover-done')).toHaveAttribute('data-device-signed', 'true');
		await expect(page.getByTestId('recover-done')).toContainText(/open as before/i);

		// And the fact that matters to everyone *else*: the homeserver carries
		// a signature on this device by the account's own self-signing key, so
		// the user's other devices and the Sensor will trust it. Asked of the
		// homeserver rather than of the browser, because that is where the
		// answer is authoritative.
		await expectDeviceIsCrossSigned(stack, password);

		// Even here — where the user *typed* the key — it never leaves the
		// page: what travels is what the key decrypts, not the key.
		await assertKeyNeverLeft(recoveryKey);
	});
});
