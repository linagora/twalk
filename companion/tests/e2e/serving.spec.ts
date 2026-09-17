// The routing contract between the build and the Gateway.
//
// Every assertion here corresponds to a line of
// `companion-gateway/src/static_files.rs`, exercised against the real export
// through `tests/serve-like-gateway.mjs`. Getting one of these wrong is the
// expensive kind of mistake ticket #66 names: a static export that cannot be
// served from the Gateway.

import { mkdir, rm, writeFile } from 'node:fs/promises';
import { gzipSync, brotliCompressSync } from 'node:zlib';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test } from '@playwright/test';

const build = join(dirname(fileURLToPath(import.meta.url)), '..', '..', 'build');

test.describe('the build as the Gateway resolves it', () => {
	test('a prerendered page resolves through its .html file', async ({ request }) => {
		// `trailingSlash: 'never'` writes `diagnostics.html`, which is step 2
		// of the Gateway's order. This is the half of the contract the
		// Companion chose.
		const response = await request.get('/diagnostics');
		expect(response.status()).toBe(200);
		expect(response.headers()['content-type']).toBe('text/html; charset=utf-8');
		expect(await response.text()).toContain('<!doctype html>');
	});

	test('the slashed spelling redirects onto the one the build has', async ({ request }) => {
		// Step 3: the build has `diagnostics.html`, the request has a slash.
		const response = await request.get('/diagnostics/', { maxRedirects: 0 });
		expect(response.status()).toBe(307);
		expect(response.headers()['location']).toBe('/diagnostics');
	});

	test('the root serves the prerendered homepage, not the fallback', async ({ request }) => {
		const response = await request.get('/');
		expect(response.status()).toBe(200);
		// `index.html` exists, so the fallback is never reached for `/`. The
		// two files differ: only the fallback is the bare shell.
		expect(await response.text()).toContain('<!doctype html>');
	});

	test('an unknown deep link is answered with the fallback at HTTP 200', async ({ page }) => {
		// The contract: `200.html` with status 200, never a 404 and never a
		// redirect, so a deep link reloaded cold loads the app. What the user
		// then sees is the app's own error screen, from the client router.
		const response = await page.goto('/networks/whatsapp/qr');
		expect(response?.status()).toBe(200);
		await expect(page.getByTestId('screen-error')).toBeVisible();
		await expect(page.getByTestId('app')).toBeVisible();
	});

	test('a path that climbs out of the build gets the fallback, not a file', async ({ request }) => {
		for (const escape of ['/../../etc/passwd', '/%2e%2e%2f%2e%2e%2fetc%2fpasswd']) {
			const response = await request.get(escape, { maxRedirects: 0 });
			expect(response.status(), escape).toBe(200);
			expect(await response.text(), escape).toContain('<!doctype html>');
		}
	});

	test('under /api a refusal stays JSON, never the app shell', async ({ request }) => {
		// A client parsing an API response must never be handed HTML.
		const response = await request.get('/api/session');
		expect(response.status()).toBe(401);
		expect(response.headers()['content-type']).toContain('application/json');
		expect(await response.json()).toMatchObject({ error: 'unauthenticated' });
	});

	test('the manifest and the icons are served with their own types', async ({ request }) => {
		const manifest = await request.get('/manifest.webmanifest');
		expect(manifest.status()).toBe(200);
		expect(manifest.headers()['content-type']).toBe('application/manifest+json');

		const icon = await request.get('/icons/icon-512.png');
		expect(icon.status()).toBe(200);
		expect(icon.headers()['content-type']).toBe('image/png');
	});

	test('the fingerprinted assets are pre-compressed', async ({ request }) => {
		// `precompress: true` writes the `.br` sibling the Gateway's
		// `ServeFile::precompressed_br` looks for. The ~1.3 MB brotli Matrix
		// crypto WebAssembly of ADR 0014 is why this matters on a phone.
		const response = await request.get('/200.html', {
			headers: { 'accept-encoding': 'br' }
		});
		expect(response.status()).toBe(200);
		expect(response.headers()['content-encoding']).toBe('br');
		expect(response.headers()['content-type']).toBe('text/html; charset=utf-8');
	});
});

test.describe('WebAssembly’s content type', () => {
	// Serial, and so in one worker: these tests share a file they create and
	// remove, and under `fullyParallel` a second worker's `afterAll` would
	// delete it from under the first.
	test.describe.configure({ mode: 'serial' });

	// `WebAssembly.instantiateStreaming` rejects anything but exactly
	// `application/wasm`, with a `TypeError` and no fallback path — a
	// `charset` parameter alone breaks onboarding, with an error that looks
	// nothing like a MIME problem. The crypto stack lands with ticket #67;
	// this proves the serving contract it will depend on, now.
	const wasm = join(build, '_app', 'immutable', 'content-type-probe.wasm');
	// The eight bytes of an empty WebAssembly module: magic, then version.
	const module = Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

	test.beforeAll(async () => {
		await mkdir(dirname(wasm), { recursive: true });
		await writeFile(wasm, module);
		await writeFile(`${wasm}.br`, brotliCompressSync(module));
		await writeFile(`${wasm}.gz`, gzipSync(module));
	});

	test.afterAll(async () => {
		await rm(wasm, { force: true });
		await rm(`${wasm}.br`, { force: true });
		await rm(`${wasm}.gz`, { force: true });
	});

	test('is exactly application/wasm, with no parameters', async ({ request }) => {
		const response = await request.get('/_app/immutable/content-type-probe.wasm', {
			headers: { 'accept-encoding': 'identity' }
		});
		expect(response.status()).toBe(200);
		expect(response.headers()['content-type']).toBe('application/wasm');
	});

	test('stays application/wasm when a compressed sibling goes out', async ({ request }) => {
		// The encoding changes; the content type is the original extension's.
		const response = await request.get('/_app/immutable/content-type-probe.wasm', {
			headers: { 'accept-encoding': 'br' }
		});
		expect(response.status()).toBe(200);
		expect(response.headers()['content-encoding']).toBe('br');
		expect(response.headers()['content-type']).toBe('application/wasm');
	});

	test('is a type the browser will actually instantiate', async ({ page }) => {
		// The assertion that matters: not the header's spelling but that
		// `instantiateStreaming` accepts it, which is the call ADR 0014's
		// crypto stack makes.
		await page.goto('/');
		const instantiated = await page.evaluate(async () => {
			try {
				await WebAssembly.instantiateStreaming(
					fetch('/_app/immutable/content-type-probe.wasm')
				);
				return 'ok';
			} catch (error) {
				return String(error);
			}
		});
		expect(instantiated).toBe('ok');
	});
});
