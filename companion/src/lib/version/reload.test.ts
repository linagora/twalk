// The order the purge runs in, and the boot on which it sticks (#442).
//
// Playwright drives the whole thing against a served build; this is the part
// where getting it wrong is a cache that comes back behind you — which is
// what happened, two times in five, in the **required** `companion` suite.

import { afterEach, describe, expect, it, vi } from 'vitest';

import { reloadForNewGateway } from './reload';

/**
 * A browser, as much of one as this function touches: the global `caches`, a
 * service-worker registry, a session store and a `location.reload` that
 * records rather than navigates. Every call is appended to one list, which is
 * how the order is asserted.
 */
function fakeBrowser(options: { marker?: string | null; registrations?: number } = {}) {
	const calls: string[] = [];
	let marker = options.marker ?? null;
	const cacheNames = ['twalk-companion-1791123278168', 'something-else'];

	const caches = {
		keys: async (): Promise<string[]> => [...cacheNames],
		delete: async (name: string): Promise<boolean> => {
			calls.push(`caches.delete(${name})`);
			return true;
		}
	};
	const serviceWorker = {
		getRegistrations: async () =>
			Array.from({ length: options.registrations ?? 1 }, (_, index) => ({
				unregister: async (): Promise<boolean> => {
					calls.push(`unregister(${index})`);
					return true;
				}
			}))
	};
	const window = {
		caches,
		sessionStorage: {
			getItem: (): string | null => marker,
			setItem: (_key: string, value: string): void => {
				marker = value;
				calls.push(`marker=${value}`);
			}
		},
		location: {
			reload: (): void => {
				calls.push('reload');
			}
		}
	};

	vi.stubGlobal('window', window);
	vi.stubGlobal('caches', caches);
	// Node 22 defines `navigator` itself, and it is read-only.
	Object.defineProperty(globalThis, 'navigator', {
		value: { serviceWorker },
		configurable: true,
		writable: true
	});
	return { calls };
}

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('acting on a version mismatch', () => {
	/**
	 * **The order is the bug.** The worker that re-populates the cache on a
	 * fetch miss keeps controlling the page until it unloads, so a purge that
	 * runs while it is still registered can be undone by any fingerprinted
	 * request the page makes next — measured: one chunk, the landing page's
	 * `$lib/crypto/store` import, back in the cache of the build we had just
	 * emptied, with `registrations` already at 0.
	 *
	 * Dropping the worker first does not close that window — an unregistered
	 * worker still serves its clients — but it is the half that decides
	 * whether the *next* navigation comes from the Gateway, and it must not be
	 * the half that waits.
	 */
	it('drops the worker before emptying the caches', async () => {
		const { calls } = fakeBrowser();

		await expect(reloadForNewGateway('9.9.9')).resolves.toBe('reloading');

		const dropped = calls.indexOf('unregister(0)');
		const purged = calls.findIndex((call) => call.startsWith('caches.delete('));
		expect(dropped).toBeGreaterThan(-1);
		expect(purged).toBeGreaterThan(-1);
		expect(dropped).toBeLessThan(purged);
		// And the reload is last, after both.
		expect(calls[calls.length - 1]).toBe('reload');
	});

	it('purges only the caches this build owns', async () => {
		const { calls } = fakeBrowser();

		await reloadForNewGateway('9.9.9');

		expect(calls).toContain('caches.delete(twalk-companion-1791123278168)');
		expect(calls).not.toContain('caches.delete(something-else)');
	});

	/**
	 * The boot where a purge sticks, and the whole of #442's fix.
	 *
	 * On the second sight of the same Gateway version nothing is registered
	 * any more — this path returns before `registerServiceWorker` — so there
	 * is no worker left to put anything back, and the purge is final. It is
	 * awaited before the banner is shown (`$lib/boot.ts`), which is why the
	 * end state the e2e test asserts is reached by construction rather than
	 * by winning a race.
	 */
	it('purges again on the boot that refuses to reload, where nothing can undo it', async () => {
		const { calls } = fakeBrowser({ marker: '9.9.9' });

		await expect(reloadForNewGateway('9.9.9')).resolves.toBe('already-tried');

		expect(calls).toContain('caches.delete(twalk-companion-1791123278168)');
		// And it does not reload: that is what "already tried" means.
		expect(calls).not.toContain('reload');
	});

	it('reloads once per Gateway version, and remembers which', async () => {
		const { calls } = fakeBrowser();

		await reloadForNewGateway('9.9.9');

		expect(calls).toContain('marker=9.9.9');
		expect(calls.filter((call) => call === 'reload')).toHaveLength(1);
	});
});
