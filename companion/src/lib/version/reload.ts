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
 * Drops the service worker, purges the caches it owns and reloads — once per
 * Gateway version. On the *second* sight of the same version it purges again
 * and says so instead of reloading.
 *
 * **Dropping the worker is the half that matters, and it goes first.** A
 * registered worker intercepts the navigation the reload triggers and would
 * answer it from the cache we just emptied with whatever it fetched next;
 * letting it go means the next load comes from the Gateway, and the fresh
 * shell registers its own worker.
 *
 * **And a purge from a page the worker still controls cannot be final** —
 * which is #442, measured rather than reasoned. An unregistered worker keeps
 * serving its existing clients until they unload, and this one re-populates
 * its cache on a fetch miss (`src/service-worker.ts`), so any fingerprinted
 * request the page makes between the purge and the unload puts a file back.
 * Two runs in five left exactly one entry behind — the landing page's
 * `$lib/crypto/store` import, whose chunk lands while this function is
 * finishing — in the cache of the build we had just emptied, with the
 * registration already gone.
 *
 * So the purge runs twice, and the second one is the one that sticks: on the
 * boot that refuses to reload, nothing is registered any more (this path
 * returns before `registerServiceWorker`), nothing can put anything back, and
 * `$lib/boot.ts` awaits it before the banner appears.
 */
export async function reloadForNewGateway(actual: string): Promise<ReloadDecision> {
	if (readMarker() === actual) {
		await purgeTheShell();
		return 'already-tried';
	}
	writeMarker(actual);

	if ('serviceWorker' in navigator) {
		try {
			const registrations = await navigator.serviceWorker.getRegistrations();
			await Promise.all(registrations.map((registration) => registration.unregister()));
		} catch {
			// Nothing registered, nothing to drop.
		}
	}

	await purgeTheShell();

	window.location.reload();
	return 'reloading';
}

/** Every cache this build's worker owns, and nothing else on the origin. */
async function purgeTheShell(): Promise<void> {
	if (!('caches' in window)) {
		// A browser that refuses the Cache API has nothing stale to hold.
		return;
	}
	try {
		const names = await caches.keys();
		await Promise.all(
			names.filter((name) => name.startsWith('twalk-companion-')).map((name) => caches.delete(name))
		);
	} catch {
		// Same.
	}
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
