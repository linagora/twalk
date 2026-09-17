// The service worker: installability, and nothing more.
//
// It caches the *fingerprinted* build alone — the files under `_app/immutable/`
// whose names contain their own content hash — so a cached response can never
// be the wrong version of a file. `$service-worker`'s `files` (whatever is in
// `static/`) and `prerendered` (the HTML shells) are deliberately not cached:
// they keep their names across builds, so a cached copy of one is exactly the
// stale shell the version handshake exists to get rid of.
//
// It performs **no offline writes** and queues nothing. Spec #65 gives the
// reason: a consent decision queued offline could be replayed hours later
// against state that has changed, which is the one thing the Companion must
// never do. So every non-GET request, and every `/api` request of any method,
// goes to the network and fails honestly when the network is not there.
//
// Registration is by hand, from `$lib/version/reload.ts`, after the handshake
// against the Gateway's `/health` has matched (`serviceWorker.register` is
// switched off in `vite.config.ts`). A shell about to be replaced must not
// install a worker that would keep serving it.

/// <reference types="@sveltejs/kit" />
/// <reference lib="webworker" />

import { build, version } from '$service-worker';

const worker = self as unknown as ServiceWorkerGlobalScope;

/**
 * One cache per build. `version` changes on every build, so activating a new
 * worker drops the previous cache whole rather than reconciling it — and the
 * `twalk-companion-` prefix is what `$lib/version/reload.ts` purges.
 */
const CACHE = `twalk-companion-${version}`;

/**
 * The fingerprinted assets, and only those. `build` is SvelteKit's list of the
 * files it emitted under `_app/`; every one of them carries a content hash.
 */
const FINGERPRINTED = new Set(build);

worker.addEventListener('install', (event) => {
	event.waitUntil(
		(async () => {
			const cache = await caches.open(CACHE);
			await cache.addAll(build);
			// Take over at once: the page that registered this worker was
			// served by the Gateway, not by a previous cache, so there is no
			// half-updated state to protect.
			await worker.skipWaiting();
		})()
	);
});

worker.addEventListener('activate', (event) => {
	event.waitUntil(
		(async () => {
			for (const name of await caches.keys()) {
				if (name !== CACHE && name.startsWith('twalk-companion-')) {
					await caches.delete(name);
				}
			}
			await worker.clients.claim();
		})()
	);
});

worker.addEventListener('fetch', (event) => {
	const request = event.request;
	if (request.method !== 'GET') {
		return;
	}

	const url = new URL(request.url);
	if (url.origin !== location.origin) {
		return;
	}

	// The Gateway's own surface is never cached: `/api` because a stale
	// answer is worse than none, `/health` because it is the handshake's
	// source of truth, `/openapi.yaml` because it describes the running
	// binary.
	if (
		url.pathname === '/health' ||
		url.pathname === '/openapi.yaml' ||
		url.pathname === '/api' ||
		url.pathname.startsWith('/api/')
	) {
		return;
	}

	if (!FINGERPRINTED.has(url.pathname)) {
		// Everything else — the HTML shells, the manifest, the icons — comes
		// from the Gateway every time. Those names outlive a build, so a
		// cached copy is how an app shell goes stale.
		return;
	}

	event.respondWith(
		(async () => {
			const cache = await caches.open(CACHE);
			const cached = await cache.match(request);
			if (cached !== undefined) {
				return cached;
			}
			// A fingerprinted file we do not have: fetch it and keep it. Its
			// name pins its content, so this can never cache the wrong thing.
			const response = await fetch(request);
			if (response.ok) {
				await cache.put(request, response.clone());
			}
			return response;
		})()
	);
});
