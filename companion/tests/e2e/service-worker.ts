// Reading the service worker's state from a test.
//
// Through `expect.poll` rather than `page.waitForFunction`: the injected
// poller checks the predicate's return value for truthiness without awaiting
// it, so an `async` predicate is *always* truthy and the wait returns at once.
// That makes a test pass for the wrong reason, which is worse than a flake.

import { expect, type Page } from '@playwright/test';

const TIMEOUT = 15_000;

/** How many service workers this origin has registered. */
export function registrationCount(page: Page): Promise<number> {
	return page.evaluate(async () => (await navigator.serviceWorker.getRegistrations()).length);
}

/** The paths of everything in the Companion's caches. */
export function cachedPaths(page: Page): Promise<string[]> {
	return page.evaluate(async () => {
		const names = (await caches.keys()).filter((name) => name.startsWith('twalk-companion-'));
		const paths: string[] = [];
		for (const name of names) {
			const cache = await caches.open(name);
			for (const request of await cache.keys()) {
				paths.push(new URL(request.url).pathname);
			}
		}
		return paths;
	});
}

/** Waits until the worker has registered and its install has filled the cache. */
export async function waitForCachedShell(page: Page): Promise<string[]> {
	await expect.poll(() => registrationCount(page), { timeout: TIMEOUT }).toBeGreaterThan(0);
	await expect
		.poll(async () => (await cachedPaths(page)).length, { timeout: TIMEOUT })
		.toBeGreaterThan(0);
	return cachedPaths(page);
}
