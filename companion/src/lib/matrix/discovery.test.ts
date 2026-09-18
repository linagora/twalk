// Screen 1's "We could not reach `<domain>`" state, and the delegation the
// Matrix spec says a client must follow before it decides that. `fetch` is a
// parameter of `discoverHomeserver`, so all of this is asserted without a
// browser and without a homeserver.

import { describe, expect, it } from 'vitest';

import { directBaseUrl, discoverHomeserver, type Fetch } from './discovery';

function respond(routes: Record<string, { status?: number; body?: unknown }>): Fetch {
	return (async (input: RequestInfo | URL) => {
		const url = String(input);
		const route = routes[url];
		if (route === undefined) {
			throw new TypeError(`Failed to fetch: ${url}`);
		}
		const status = route.status ?? 200;
		return new Response(JSON.stringify(route.body ?? {}), {
			status,
			headers: { 'content-type': 'application/json' }
		});
	}) as Fetch;
}

describe('discoverHomeserver', () => {
	it('uses the domain itself when there is no delegation', () => {
		// A 404 from `.well-known` is the spec's IGNORE case, not a failure.
		return expect(
			discoverHomeserver(
				'example.com',
				respond({
					'https://example.com/.well-known/matrix/client': { status: 404 },
					'https://example.com/_matrix/client/versions': { body: { versions: ['v1.11'] } }
				})
			)
		).resolves.toEqual({
			ok: true,
			homeserver: { baseUrl: 'https://example.com', versions: ['v1.11'], delegated: false }
		});
	});

	it('follows `m.homeserver.base_url` where there is one', async () => {
		const result = await discoverHomeserver(
			'example.com',
			respond({
				'https://example.com/.well-known/matrix/client': {
					body: { 'm.homeserver': { base_url: 'https://matrix.example.com/' } }
				},
				'https://matrix.example.com/_matrix/client/versions': {
					body: { versions: ['v1.11'] }
				}
			})
		);
		expect(result).toMatchObject({
			ok: true,
			homeserver: { baseUrl: 'https://matrix.example.com', delegated: true }
		});
	});

	it('ignores a `.well-known` the browser could not read', async () => {
		// A marketing site with no CORS header fails the fetch outright, and
		// the spec's answer is still "use the domain".
		const result = await discoverHomeserver(
			'example.com',
			respond({
				'https://example.com/_matrix/client/versions': { body: { versions: ['v1.11'] } }
			})
		);
		expect(result).toMatchObject({ ok: true, homeserver: { delegated: false } });
	});

	it('speaks plain HTTP to a loopback deployment', async () => {
		// The developer's machine and the Playwright harness. Loopback is a
		// secure context by browser rule, so there is no certificate to have.
		const result = await discoverHomeserver(
			'twalk.localhost:19148',
			respond({
				'http://twalk.localhost:19148/_matrix/client/versions': {
					body: { versions: ['v1.11'] }
				}
			})
		);
		expect(result).toMatchObject({
			ok: true,
			homeserver: { baseUrl: 'http://twalk.localhost:19148' }
		});
	});

	it('reports a domain that answers nothing as unreachable', async () => {
		const result = await discoverHomeserver('nowhere.example', respond({}));
		expect(result).toMatchObject({ ok: false, kind: 'unreachable' });
	});

	it('reports a domain that answers something else as not a homeserver', async () => {
		const result = await discoverHomeserver(
			'example.com',
			respond({
				'https://example.com/_matrix/client/versions': { body: { hello: 'world' } }
			})
		);
		expect(result).toMatchObject({ ok: false, kind: 'not-a-homeserver' });
	});

	it('treats a homeserver’s own 5xx as unreachable, not as a wrong address', async () => {
		const result = await discoverHomeserver(
			'example.com',
			respond({
				'https://example.com/_matrix/client/versions': { status: 502 }
			})
		);
		expect(result).toMatchObject({ ok: false, kind: 'unreachable' });
	});
});

describe('directBaseUrl', () => {
	it('spells a bare server name the way this project spells one', () => {
		// What a user can recite: the half of their own Matrix ID after the
		// colon. `linagora.com` failing while `https://matrix.linagora.com`
		// worked is Matrix's delegation mechanism inverted (#124).
		expect(directBaseUrl('linagora.com')).toBe('https://linagora.com');
		expect(directBaseUrl('  Example.COM ')).toBe('https://example.com');
		expect(directBaseUrl('twalk.localhost:19148')).toBe('http://twalk.localhost:19148');
	});

	it('takes an address the user gave as the address it is', () => {
		// Port and path included: `homeserverBaseUrl` alone drops a port on
		// anything but loopback, and a homeserver on 8448 is a real deployment.
		expect(directBaseUrl('https://matrix.example.com')).toBe('https://matrix.example.com');
		expect(directBaseUrl('https://matrix.example.com:8448')).toBe('https://matrix.example.com:8448');
		expect(directBaseUrl('https://example.com/synapse/')).toBe('https://example.com/synapse');
		expect(directBaseUrl('http://127.0.0.1:19148')).toBe('http://127.0.0.1:19148');
	});
});
