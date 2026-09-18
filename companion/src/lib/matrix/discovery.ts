// Finding the user's homeserver behind the domain they typed on screen 1.
//
// This is Matrix's own client-side discovery
// (https://spec.matrix.org/v1.11/client-server-api/#server-discovery), not an
// invention: `GET https://<domain>/.well-known/matrix/client`, read
// `m.homeserver.base_url`, and verify whatever came out by asking it
// `/_matrix/client/versions`. A `.well-known` that is missing, unreadable or
// cross-origin-blocked is the spec's FAIL_PROMPT/IGNORE case and means "use
// the domain itself", which is why every failure here falls through instead of
// stopping.
//
// The verification step is the one that matters for screen 1: the wireframe's
// *homeserver unreachable* state is defined by a homeserver that does not
// answer, and `/_matrix/client/versions` is the only endpoint every homeserver
// answers unauthenticated. It is also what tells us, before an account is
// created, that the thing at that domain is a Matrix homeserver and not a
// parked page.
//
// # A server name or an address
//
// The value this takes is **what a user typed**, and Matrix's delegation
// mechanism exists precisely so that what they type can be their server name —
// `linagora.com`, the half of their own Matrix ID they can recite — rather than
// the address of the machine behind it. The networks screen asked for a base
// URL and got a failure for `linagora.com` while `https://matrix.linagora.com`
// worked, which is the mechanism inverted (#124). So both spellings are
// accepted here: a bare name is turned into an address the way screen 1 does,
// and a value that already carries a scheme is taken as the address it is —
// port, path and all, which `homeserverBaseUrl` alone would drop.
//
// `fetch` is a parameter so this module is testable without a browser; the
// callers pass none and get the global.

import { homeserverBaseUrl } from '$lib/onboarding/domain';

export type Fetch = typeof globalThis.fetch;

export interface DiscoveredHomeserver {
	/** Where to point matrix-js-sdk. No trailing slash. */
	baseUrl: string;
	/** The `/_matrix/client/versions` the homeserver advertises. */
	versions: string[];
	/** Whether a `.well-known` delegation sent us somewhere else. */
	delegated: boolean;
}

export type DiscoveryFailure =
	/** Nothing answered at that domain: DNS, TLS, the network, or a 5xx. */
	| { kind: 'unreachable'; detail: string }
	/** Something answered, but it is not a Matrix homeserver. */
	| { kind: 'not-a-homeserver'; detail: string };

export type Discovery = { ok: true; homeserver: DiscoveredHomeserver } | ({ ok: false } & DiscoveryFailure);

/** How long screen 1 waits before calling a domain unreachable. */
const TIMEOUT_MS = 8000;

/**
 * Resolves a domain to a homeserver, or says which of the wireframe's two
 * failures happened.
 */
export async function discoverHomeserver(domain: string, fetchImpl?: Fetch): Promise<Discovery> {
	const doFetch = fetchImpl ?? globalThis.fetch.bind(globalThis);
	const direct = directBaseUrl(domain);
	const delegated = await readWellKnown(direct, doFetch);
	const baseUrl = delegated ?? direct;

	let response: Response;
	try {
		response = await withTimeout(
			(signal) =>
				doFetch(`${baseUrl}/_matrix/client/versions`, {
					signal,
					// No credentials, no cookies: this is somebody else's origin.
					credentials: 'omit'
				}),
			TIMEOUT_MS
		);
	} catch (cause) {
		return {
			ok: false,
			kind: 'unreachable',
			detail: cause instanceof Error ? cause.message : 'the request failed'
		};
	}

	if (!response.ok) {
		return {
			ok: false,
			kind: response.status >= 500 ? 'unreachable' : 'not-a-homeserver',
			detail: `${baseUrl}/_matrix/client/versions answered ${response.status}`
		};
	}

	let body: unknown;
	try {
		body = await response.json();
	} catch {
		return {
			ok: false,
			kind: 'not-a-homeserver',
			detail: 'the versions endpoint did not answer JSON'
		};
	}
	const versions = readVersions(body);
	if (versions === null) {
		return {
			ok: false,
			kind: 'not-a-homeserver',
			detail: 'the versions endpoint answered no version list'
		};
	}

	return {
		ok: true,
		homeserver: { baseUrl, versions, delegated: delegated !== null }
	};
}

/**
 * The address to ask before delegation is consulted: what the user typed, read
 * as an address when it is one and as a server name when it is not.
 *
 * A scheme is the tell. `https://matrix.example.com:8448/synapse` is somebody
 * naming a homeserver exactly, and every part of it matters;
 * `matrix.example.com` is a name, and `homeserverBaseUrl` knows how this
 * project spells one (HTTPS, except on loopback, where a deployment has no
 * certificate and needs none).
 */
export function directBaseUrl(value: string): string {
	const trimmed = value.trim();
	if (!/^https?:\/\//i.test(trimmed)) {
		return homeserverBaseUrl(trimmed);
	}
	try {
		const url = new URL(trimmed);
		const path = url.pathname === '/' ? '' : url.pathname.replace(/\/+$/, '');
		return `${url.origin}${path}`;
	} catch {
		// Not a URL after all; let the name spelling have it.
		return homeserverBaseUrl(trimmed);
	}
}

/**
 * The delegation, or `null` for every way there can fail to be one — a 404, a
 * body that is not a delegation, a CORS refusal on somebody's marketing site.
 * The spec's own answer to all of them is the same: use the domain.
 */
async function readWellKnown(direct: string, doFetch: Fetch): Promise<string | null> {
	try {
		const response = await withTimeout(
			(signal) =>
				doFetch(`${direct}/.well-known/matrix/client`, { signal, credentials: 'omit' }),
			TIMEOUT_MS
		);
		if (!response.ok) {
			return null;
		}
		const body: unknown = await response.json();
		const base = (body as { 'm.homeserver'?: { base_url?: unknown } })?.['m.homeserver']
			?.base_url;
		if (typeof base !== 'string' || base.length === 0) {
			return null;
		}
		const url = new URL(base);
		if (url.protocol !== 'https:' && url.protocol !== 'http:') {
			return null;
		}
		return `${url.origin}${url.pathname === '/' ? '' : url.pathname.replace(/\/+$/, '')}`;
	} catch {
		return null;
	}
}

function readVersions(body: unknown): string[] | null {
	const versions = (body as { versions?: unknown })?.versions;
	if (!Array.isArray(versions)) {
		return null;
	}
	const strings = versions.filter((version): version is string => typeof version === 'string');
	return strings.length === 0 ? null : strings;
}

/**
 * `AbortSignal.timeout` is not in every browser of the baseline, and a
 * discovery probe that never answers is the failure this whole module exists
 * to turn into a sentence.
 */
async function withTimeout(
	run: (signal: AbortSignal) => Promise<Response>,
	milliseconds: number
): Promise<Response> {
	const controller = new AbortController();
	const timer = setTimeout(() => controller.abort(), milliseconds);
	try {
		return await run(controller.signal);
	} finally {
		clearTimeout(timer);
	}
}
