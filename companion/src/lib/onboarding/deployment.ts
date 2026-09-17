// Screen 1's "Continue", after the hostname has passed validation: is there a
// Twalk deployment here, and where has the journey already got to?
//
// Two questions, asked of two different things, and the order is deliberate:
//
//   1. **The Gateway**, at the Companion's own origin — it is the deployment's
//      front door and it serves these very files. `GET /api/session` answers
//      all three states that matter before an account exists: `401` (open, no
//      account signed in), `200` (this browser already holds a session, so the
//      user is returning), and `503 sign_in_not_configured` (the deployment
//      has no owner: the wireframe's "does not allow new accounts", which is
//      an operator's problem and not a retry).
//   2. **The homeserver**, behind the domain the user typed — Matrix's own
//      discovery (`$lib/matrix/discovery.ts`). This is the wireframe's
//      "We could not reach `<domain>`" state, and it is asked second because
//      a deployment whose API is closed cannot be onboarded whatever its
//      homeserver says.
//
// The answer also carries where the journey resumes, because spec #65 is
// explicit that onboarding progress is derived from what exists on the
// Gateway — never from a flag this app wrote into browser storage.

import { gateway } from '$lib/api/client';
import { discoverHomeserver, type DiscoveredHomeserver } from '$lib/matrix/discovery';

/** What screen 1 does next. */
export type DeploymentOutcome =
	/**
	 * Nobody is signed in and the API is open: go to screen 2 and create the
	 * account.
	 */
	| { kind: 'ready'; homeserver: DiscoveredHomeserver }
	/**
	 * This browser holds a live Gateway session. Whether onboarding continues
	 * or the store-loss journey starts depends on the crypto store, which
	 * `$lib/crypto/store.ts` answers — not this module.
	 */
	| { kind: 'signed-in'; homeserver: DiscoveredHomeserver; owner: string }
	/** `GATEWAY_OWNER` is unset: this deployment accepts nobody. */
	| { kind: 'not-configured'; detail: string }
	/** The Gateway itself did not answer. Retry is the right offer. */
	| { kind: 'gateway-unreachable'; detail: string }
	/** The domain answered nothing, or nothing Matrix-shaped. */
	| { kind: 'homeserver-unreachable'; domain: string; detail: string }
	| { kind: 'not-a-homeserver'; domain: string; detail: string };

/**
 * Probes the deployment for the domain the user typed. Never throws: every
 * outcome is a screen the wireframes describe.
 */
export async function probeDeployment(domain: string): Promise<DeploymentOutcome> {
	let status: number;
	let owner: string | null = null;
	let detail = '';
	try {
		const result = await gateway.GET('/api/session');
		status = result.response.status;
		owner = result.data?.owner ?? null;
		detail = (result.error as { detail?: string } | undefined)?.detail ?? '';
	} catch (cause) {
		return {
			kind: 'gateway-unreachable',
			detail: cause instanceof Error ? cause.message : 'the request failed'
		};
	}

	if (status === 503) {
		return { kind: 'not-configured', detail };
	}
	if (status !== 200 && status !== 401) {
		return { kind: 'gateway-unreachable', detail: `/api/session answered ${status}` };
	}

	const discovery = await discoverHomeserver(domain);
	if (!discovery.ok) {
		return discovery.kind === 'unreachable'
			? { kind: 'homeserver-unreachable', domain, detail: discovery.detail }
			: { kind: 'not-a-homeserver', domain, detail: discovery.detail };
	}

	if (status === 200 && owner !== null) {
		return { kind: 'signed-in', homeserver: discovery.homeserver, owner };
	}
	return { kind: 'ready', homeserver: discovery.homeserver };
}
