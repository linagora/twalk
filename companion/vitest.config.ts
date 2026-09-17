// Vitest runs without the SvelteKit plugin: these are unit tests over the
// Companion's *pure* modules — the capability report, the version handshake's
// comparison, locale negotiation, hostname validation, the diagnostics text.
//
// Anything that touches a browser API, the Gateway or crypto is Playwright's
// (`tests/e2e/`), as spec #65 decided. Keeping the plugin out is what enforces
// that: a module importing `$app/*` or reaching for `indexedDB` at module
// scope simply will not load here.

import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

export default defineConfig({
	resolve: {
		alias: {
			$lib: fileURLToPath(new URL('./src/lib', import.meta.url))
		}
	},
	test: {
		environment: 'node',
		include: ['src/**/*.test.ts']
	}
});
