// The Companion's build. Since SvelteKit 2.62 the framework's own options are
// passed to the `sveltekit()` plugin rather than kept in a `svelte.config.js`,
// so this file is the whole build configuration.
//
// What matters here, and why:
//
//   - `adapter-static` with `fallback: '200.html'` and `precompress: true`.
//     The Gateway serves this export (`GATEWAY_STATIC_DIR`) and answers any
//     path the build has no file for with `GATEWAY_FALLBACK_FILE`, whose
//     default is exactly `200.html`, at HTTP 200 — so a deep link reloaded
//     cold loads the app instead of a 404. `precompress` writes the `.br` and
//     `.gz` siblings the Gateway's `ServeFile` looks for; the ~1.3 MB brotli
//     Matrix crypto WebAssembly of ADR 0014 is the reason that matters.
//
//   - `trailingSlash` is **not** set here, because it is a page option, not a
//     build option: it lives in `src/routes/+layout.ts`. We keep SvelteKit's
//     default, `'never'`, which writes a prerendered page as `<path>.html`.
//     That is step 2 of the Gateway's resolution order
//     (`companion-gateway/src/static_files.rs`), and the Gateway's step 4
//     redirects the slashed spelling onto it, so the two agree.
//
//   - `serviceWorker.register: false`. The service worker is registered by
//     hand, after the version handshake against the Gateway's `/health` has
//     run (`src/lib/version/handshake.ts`): a shell whose version no longer
//     matches the Gateway must reload before it installs a worker that would
//     keep serving it.

import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, type Plugin } from 'vite';

/**
 * Strips the debug sections from every `.wasm` the build emits, which in
 * practice means the Matrix crypto stack of ADR 0014 — the ADR's "debug
 * symbols stripped, brotli served".
 *
 * `@matrix-org/matrix-sdk-crypto-wasm` ships 7.8 MB of WebAssembly, 3.0 MB of
 * which is the `name` custom section: the Rust symbol names, which a browser
 * uses for nothing but a stack trace. Dropping it and the two other custom
 * sections takes the module to about 4.8 MB, and the brotli sibling
 * `precompress` writes beside it from roughly 1.45 MB to 1.3 MB — the figure
 * the ADR budgets for.
 *
 * Done here rather than with `wasm-strip` or `wasm-opt` because it needs no
 * toolchain: a WebAssembly module is a header and a list of sections, custom
 * sections carry an id of 0 and a name, and removing whole sections is a copy.
 * Nothing is rewritten, so nothing can be rewritten wrongly.
 */
function stripWasmDebugSections(): Plugin {
	const DROPPED = new Set(['name', 'producers', 'target_features']);

	return {
		name: 'twalk:strip-wasm-debug-sections',
		apply: 'build',
		generateBundle(_options, bundle) {
			for (const chunk of Object.values(bundle)) {
				if (chunk.type !== 'asset' || !chunk.fileName.endsWith('.wasm')) {
					continue;
				}
				const source = chunk.source;
				if (typeof source === 'string') {
					continue;
				}
				const stripped = stripSections(new Uint8Array(source), DROPPED);
				if (stripped !== null) {
					chunk.source = stripped;
				}
			}
		}
	};
}

/** `null` when the bytes are not a WebAssembly module we recognise. */
function stripSections(module: Uint8Array, dropped: Set<string>): Uint8Array | null {
	// `\0asm` and version 1, then sections until the end.
	if (
		module.length < 8 ||
		module[0] !== 0x00 ||
		module[1] !== 0x61 ||
		module[2] !== 0x73 ||
		module[3] !== 0x6d
	) {
		return null;
	}
	const kept: Uint8Array[] = [module.subarray(0, 8)];
	let at = 8;
	while (at < module.length) {
		const start = at;
		const id = module[at];
		at += 1;
		const [size, afterSize] = leb128(module, at);
		if (size === null || afterSize === null) {
			return null;
		}
		at = afterSize;
		const body = at;
		at += size;
		if (at > module.length) {
			return null;
		}
		if (id === 0) {
			const [nameLength, afterNameLength] = leb128(module, body);
			if (nameLength !== null && afterNameLength !== null) {
				const name = new TextDecoder().decode(
					module.subarray(afterNameLength, afterNameLength + nameLength)
				);
				if (dropped.has(name)) {
					continue;
				}
			}
		}
		kept.push(module.subarray(start, at));
	}

	const total = kept.reduce((sum, part) => sum + part.length, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const part of kept) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}

/** An unsigned LEB128, returning the value and the offset after it. */
function leb128(bytes: Uint8Array, at: number): [number | null, number | null] {
	let result = 0;
	let shift = 0;
	let cursor = at;
	for (;;) {
		if (cursor >= bytes.length || shift > 35) {
			return [null, null];
		}
		const byte = bytes[cursor] ?? 0;
		cursor += 1;
		result |= (byte & 0x7f) << shift;
		if ((byte & 0x80) === 0) {
			return [result >>> 0, cursor];
		}
		shift += 7;
	}
}

export default defineConfig({
	plugins: [
		stripWasmDebugSections(),
		sveltekit({
			compilerOptions: {
				// Runes everywhere except in dependencies. Svelte 5's default
				// for new projects; drop it in Svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},

			adapter: adapter({
				fallback: '200.html',
				precompress: true
			}),

			serviceWorker: {
				register: false
			}
		})
	]
});
