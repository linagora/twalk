// The version handshake.
//
// The Companion is installable, so a service worker can keep serving an app
// shell that was built against an older Gateway — for days, on a phone the
// user came back to. The Gateway's `/health` exists partly for this: its
// `version` is asserted, by the Gateway's own conformance test, to equal the
// `info.version` of `companion-gateway/openapi.yaml`, which is the value
// `scripts/generate-api-client.mjs` baked into this build as
// `EXPECTED_GATEWAY_VERSION`. So the two are the same number on a healthy
// deployment, and a difference means exactly one thing: this shell is stale.
//
// This module decides; `./reload.ts` acts, because reloading is browser-only
// and the decision is worth testing on its own.

import { gateway } from '$lib/api/client';
import { EXPECTED_GATEWAY_VERSION } from '$lib/api/gateway-version';

export { EXPECTED_GATEWAY_VERSION };

export type Handshake =
	/** No answer yet. */
	| { kind: 'checking' }
	/** The Gateway runs the version this build was made for. */
	| { kind: 'match'; version: string }
	/** The Gateway moved. The shell must be replaced. */
	| { kind: 'mismatch'; expected: string; actual: string }
	/**
	 * The Gateway is the right version and ships a **different build of the
	 * Companion** than the one running (#222): this browser holds a shell the
	 * server no longer serves — a redeploy an open tab never picked up. The
	 * shell must be replaced, the same way.
	 */
	| { kind: 'stale-shell'; running: string; shipped: string }
	/** `/health` did not answer, or answered something else. Not a mismatch. */
	| { kind: 'unreachable'; reason: string };

/**
 * Compares the baked-in version with what `/health` reported.
 *
 * An absent or empty `version` is `unreachable`, not `mismatch`: a proxy
 * serving its own error page must not make the app reload in a loop.
 */
export function compareVersions(expected: string, reported: string | undefined | null): Handshake {
	if (typeof reported !== 'string' || reported.length === 0) {
		return { kind: 'unreachable', reason: 'the health response carries no version' };
	}
	if (reported === expected) {
		return { kind: 'match', version: reported };
	}
	return { kind: 'mismatch', expected, actual: reported };
}

/**
 * The second comparison (#222): the build this page runs against the build
 * the Gateway ships. A Gateway that names no build (an export without
 * `_app/version.json`, or a Gateway older than this field) cannot be compared
 * and is left at the first comparison's answer — never a stale shell on a
 * guess, for the same reason a versionless `/health` is never a mismatch.
 */
export function compareBuilds(
	running: string,
	shipped: string | null | undefined,
	otherwise: Handshake
): Handshake {
	if (typeof shipped !== 'string' || shipped.length === 0 || shipped === running) {
		return otherwise;
	}
	return { kind: 'stale-shell', running, shipped };
}

/** What `/health` told us, for the diagnostics page. */
export interface GatewayHealth {
	version: string;
	revision: string;
	/** The build of the Companion the Gateway ships, or `null` when it names none. */
	companionBuild: string | null;
}

export interface HandshakeResult {
	outcome: Handshake;
	health: GatewayHealth | null;
}

/**
 * Asks the Gateway its version and compares. Uses the generated client, so
 * the shape of the health document is the description's and not this file's
 * idea of it.
 */
export async function shakeHands(
	expected = EXPECTED_GATEWAY_VERSION,
	/**
	 * The build this page was made from: SvelteKit's `version` from
	 * `$app/environment`, which the caller supplies because that module is the
	 * app's and not the unit tests'. `null` skips the build comparison.
	 */
	running: string | null = null
): Promise<HandshakeResult> {
	try {
		const result = await gateway.GET('/health');
		const data = result.data;
		if (data === undefined) {
			return {
				outcome: {
					kind: 'unreachable',
					reason: `health answered ${result.response.status}`
				},
				health: null
			};
		}
		const versions = compareVersions(expected, data.version);
		// The build comparison only once the versions agree: a moved Gateway
		// is already a reload, and one reason at a time is one banner at a
		// time.
		const outcome =
			versions.kind === 'match' && running !== null
				? compareBuilds(running, data.companion_build, versions)
				: versions;
		return {
			outcome,
			health: {
				version: data.version,
				revision: data.revision,
				companionBuild: data.companion_build ?? null
			}
		};
	} catch (cause) {
		return {
			outcome: {
				kind: 'unreachable',
				reason: cause instanceof Error ? cause.message : 'the request failed'
			},
			health: null
		};
	}
}
