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

/** What `/health` told us, for the diagnostics page. */
export interface GatewayHealth {
	version: string;
	revision: string;
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
export async function shakeHands(expected = EXPECTED_GATEWAY_VERSION): Promise<HandshakeResult> {
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
		return {
			outcome: compareVersions(expected, data.version),
			health: { version: data.version, revision: data.revision }
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
