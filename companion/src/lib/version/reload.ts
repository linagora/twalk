// Acting on a version mismatch: throw away the cached shell and load the new
// one. Browser-only, and imported dynamically by the root layout.
//
// The reload is guarded, because an unguarded one is a reload loop. Two ways
// the loop happens:
//
//   - the Gateway is *older* than the shell (an operator rolled back, or a
//     developer is running a stale binary against a fresh build): no reload
//     will ever make the versions agree;
//   - the new shell has not reached the cache yet, so reloading serves the
//     same bytes again.
//
// So we reload at most once per observed Gateway version, remembering which in
// `sessionStorage` — per tab, cleared when the tab closes, and holding nothing
// but a version string. After that the app says so and offers the button; the
// user's own reload is not our business to suppress.

const RELOAD_MARKER = 'twalk:reloaded-for-gateway-version';

/** The outcome, so the caller can tell the user which of the two happened. */
export type ReloadDecision = 'reloading' | 'already-tried';

/**
 * Purges the caches this build's service worker owns, drops its registration
 * and reloads — once per Gateway version.
 *
 * Unregistering as well as purging matters: a registered worker intercepts the
 * navigation the reload triggers, and would answer it from the cache we just
 * emptied with whatever it fetched next. Letting it go means the next load
 * comes from the Gateway, and the fresh shell registers its own worker.
 */
export async function reloadForNewGateway(actual: string): Promise<ReloadDecision> {
	if (readMarker() === actual) {
		return 'already-tried';
	}
	writeMarker(actual);

	if ('caches' in window) {
		try {
			const names = await caches.keys();
			await Promise.all(
				names.filter((name) => name.startsWith('twalk-companion-')).map((name) => caches.delete(name))
			);
		} catch {
			// A browser that refuses the Cache API has nothing stale to hold.
		}
	}

	if ('serviceWorker' in navigator) {
		try {
			const registrations = await navigator.serviceWorker.getRegistrations();
			await Promise.all(registrations.map((registration) => registration.unregister()));
		} catch {
			// Same: nothing registered, nothing to drop.
		}
	}

	window.location.reload();
	return 'reloading';
}

function readMarker(): string | null {
	try {
		return window.sessionStorage.getItem(RELOAD_MARKER);
	} catch {
		// Storage can be switched off (the capability gate says so). Without
		// the marker we would loop, so treat it as "already tried".
		return null;
	}
}

function writeMarker(value: string): void {
	try {
		window.sessionStorage.setItem(RELOAD_MARKER, value);
	} catch {
		// See above: if we cannot remember, we must not reload. The caller
		// gets `reloading` all the same, and the page reloads once — the
		// marker's absence then shows the banner instead of looping, because
		// a browser with no sessionStorage has no service worker either
		// (Lockdown Mode), so there is no stale shell to purge.
	}
}

/**
 * Registers the service worker. Called only after the handshake matched: a
 * shell that is about to be replaced must not install a worker that would
 * keep serving it.
 *
 * SvelteKit's automatic registration is switched off in `vite.config.ts` for
 * exactly that reason.
 */
export async function registerServiceWorker(): Promise<void> {
	if (!('serviceWorker' in navigator)) {
		return;
	}
	try {
		await navigator.serviceWorker.register('/service-worker.js', { scope: '/' });
	} catch {
		// Installability is a convenience, not a requirement: the capability
		// gate already reported the worker as degraded if it cannot run.
	}
}
