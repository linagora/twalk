// What the Companion does before it shows a screen, in the order it must.
//
//   1. Pick the language, from `navigator.languages`.
//   2. Adopt whatever Gateway session this browser already holds, and start
//      keeping it alive (`$lib/session/refresh.ts`). Not awaited: no screen
//      waits on it, and the client wrapper repairs a `401` that arrives first.
//   3. Ask the browser what it can do. If something onboarding needs is
//      missing, the gate screen replaces the app — spec #65 asks for that
//      *before* onboarding starts, never in the middle of it.
//   4. Ask the Gateway its version. On a mismatch the shell is stale and
//      reloads itself; on a match it registers the service worker.
//
// Step 2 first among the network calls because it is the one that decides
// whether the rest of the app has a credential at all: a page restored from a
// background tab reaches this line with a token that died hours ago, and the
// refresh is what it needs before its first screen asks anything (#111).
//
// The version handshake comes after the capability probe on purpose: a browser
// with no service worker cannot hold a stale shell, and a browser that cannot
// store keys should be told that rather than reloaded.
//
// This module holds no browser API of its own. The probe and the reload are
// dynamically imported, so the whole boot sequence is reachable from a module
// graph that Node can load during prerendering.

import { writable } from 'svelte/store';

import type { CapabilityReport } from '$lib/capabilities/report';
import { initLocale } from '$lib/i18n';
import { startSessionKeeper } from '$lib/session/refresh';
import {
	EXPECTED_GATEWAY_VERSION,
	shakeHands,
	type GatewayHealth,
	type Handshake
} from '$lib/version/handshake';

export interface BootState {
	/** `null` until the probe answers. */
	capabilities: CapabilityReport | null;
	handshake: Handshake;
	health: GatewayHealth | null;
	/** True once the sequence has run, whatever it found. */
	ready: boolean;
	/**
	 * Set when the versions disagree and a reload has already been tried for
	 * this Gateway version: the app then says so and offers the button rather
	 * than looping. See `$lib/version/reload.ts`.
	 */
	reloadRefused: boolean;
}

const state = writable<BootState>({
	capabilities: null,
	handshake: { kind: 'checking' },
	health: null,
	ready: false,
	reloadRefused: false
});

export const boot = { subscribe: state.subscribe };

export { EXPECTED_GATEWAY_VERSION };

let started = false;

/**
 * Runs the sequence. Browser-only, idempotent, and called once from the root
 * layout's `onMount`.
 */
export async function startBoot(): Promise<void> {
	if (started) {
		return;
	}
	started = true;

	initLocale(navigator.languages ?? [navigator.language]);

	// Deliberately not awaited: the capability gate must not wait on a network
	// round trip, and every call this app makes is already repaired centrally
	// if it overtakes the refresh.
	void startSessionKeeper();

	// Dynamic: `probe.ts` reads `indexedDB` and `isSecureContext`, which do
	// not exist in the Node process that prerenders this app.
	const [{ probeCapabilities }, { reportCapabilities }] = await Promise.all([
		import('$lib/capabilities/probe'),
		import('$lib/capabilities/report')
	]);
	const capabilities = reportCapabilities(await probeCapabilities());
	state.update((current) => ({ ...current, capabilities }));

	const { outcome, health } = await shakeHands();
	state.update((current) => ({ ...current, handshake: outcome, health, ready: true }));

	const { reloadForNewGateway, registerServiceWorker } = await import('$lib/version/reload');

	if (outcome.kind === 'mismatch') {
		const decision = await reloadForNewGateway(outcome.actual);
		if (decision === 'already-tried') {
			state.update((current) => ({ ...current, reloadRefused: true }));
		}
		// `reloading` navigates away; nothing below runs.
		return;
	}

	if (capabilities.ok) {
		await registerServiceWorker();
	}
}

/** Re-runs the capability probe, for the gate screen's "check again". */
export async function recheckCapabilities(): Promise<void> {
	const [{ probeCapabilities }, { reportCapabilities }] = await Promise.all([
		import('$lib/capabilities/probe'),
		import('$lib/capabilities/report')
	]);
	const capabilities = reportCapabilities(await probeCapabilities());
	state.update((current) => ({ ...current, capabilities }));
}
