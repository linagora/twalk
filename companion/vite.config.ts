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
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
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
