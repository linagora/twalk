// A failure the homeserver never saw must not be reported as a refusal it
// made — the defect that had a user hunting for a password that was correct
// all along, and that survived its own fix because the classification was
// written in the screen while the throw was written here (#115, #117).

import { describe, expect, it } from 'vitest';

import { passwordLogin, RestoreError } from './bootstrap';

const answering = (status: number, body: unknown): typeof fetch =>
	(async () =>
		new Response(JSON.stringify(body), {
			status,
			headers: { 'content-type': 'application/json' }
		})) as unknown as typeof fetch;

async function problemOf(run: () => Promise<unknown>): Promise<string> {
	try {
		await run();
	} catch (cause) {
		expect(cause).toBeInstanceOf(RestoreError);
		return (cause as RestoreError).problem;
	}
	throw new Error('the call was expected to fail');
}

describe('passwordLogin', () => {
	const original = globalThis.fetch;
	const withFetch = async (impl: typeof fetch, run: () => Promise<unknown>) => {
		globalThis.fetch = impl;
		try {
			return await problemOf(run);
		} finally {
			globalThis.fetch = original;
		}
	};

	it('says unreachable when the request never reached a server', async () => {
		// What a wrong address, a missing port or no route produces: `fetch`
		// rejects, and nothing has seen the password.
		const rejecting = (async () => {
			throw new TypeError('Failed to fetch');
		}) as unknown as typeof fetch;
		expect(
			await withFetch(rejecting, () =>
				passwordLogin('http://twalk.example:8009', '@michel:twalk.example', 'hunter2')
			)
		).toBe('unreachable');
	});

	it('says wrong-password only when the homeserver refused it', async () => {
		expect(
			await withFetch(answering(403, { errcode: 'M_FORBIDDEN' }), () =>
				passwordLogin('http://twalk.example:8009', '@michel:twalk.example', 'hunter2')
			)
		).toBe('wrong-password');
	});

	it('keeps every other answer distinct from both', async () => {
		// A rate limit is neither the address nor the password, and saying
		// either would send the user to fix something that is not broken.
		expect(
			await withFetch(answering(429, { errcode: 'M_LIMIT_EXCEEDED' }), () =>
				passwordLogin('http://twalk.example:8009', '@michel:twalk.example', 'hunter2')
			)
		).toBe('failed');
	});
});
