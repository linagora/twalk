// The suite cannot adopt a server it did not start (#185).
//
// The defect: `tests/serve-like-gateway.mjs` listens on a fixed port, Playwright
// reuses whatever answers there, and on 2026-09-19 that was a server from a run
// started twenty-eight hours earlier. Every spec met the previous day's export.
// Thirty-six failed. `serving.spec.ts` reported a prerendered page as a 404 —
// an assertion with nothing to do with any recent change, which is why the
// natural response was to go looking for the defect in the diff.
//
// What is asserted here is the **diagnosis**, which is the thing the ticket asks
// for: given a server on the port that is not this suite's, the suite says the
// port is held and by what, rather than failing assertions somewhere else.
//
// `tests/port-guard.mjs` is run as a program, the way `playwright.config.ts` runs
// it — the only way to exercise it, since by the time a spec is running the guard
// has already decided. It needs neither Docker nor a stack, so it is in the
// Node-only project and runs in CI's required tier: this is a defence that must
// not be allowed to rot, and it is cheap enough not to.
//
// The decoy's port is **the kernel's**, asked for with `listen(0)` the way
// `tests/real-stack.mjs` asks for the Gateway's, and never one of the suite's own
// — a spec that bound 4319 would be the very defect it is testing, and a spec
// that hard-coded any port would collide with a second worktree running this same
// file, which is the class of flake this file exists for.

import { execFile } from 'node:child_process';
import { createServer, type Server } from 'node:http';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { expect, test } from '@playwright/test';

import { SERVING_PATH } from '../build-id.mjs';

const run = promisify(execFile);

const guard = join(dirname(fileURLToPath(import.meta.url)), '..', 'port-guard.mjs');

/** A build name that is this file's and no run's. */
const BUILD = 'deadbeefdeadbeef';

/**
 * Runs the guard over one port and hands back what it said and what it exited
 * with. Never throws on a refusal: the refusal is the subject.
 */
async function askGuard(build: string, mode: '--reuse' | '--exclusive', port: number) {
	try {
		const { stdout, stderr } = await run(process.execPath, [
			guard,
			build,
			mode,
			`${port}=a port this spec is holding`
		]);
		return { code: 0, stdout, stderr };
	} catch (error) {
		const failure = error as { code?: number; stdout?: string; stderr?: string };
		return { code: failure.code ?? 1, stdout: failure.stdout ?? '', stderr: failure.stderr ?? '' };
	}
}

/**
 * A listener that answers something, on a port the kernel chose, closed whatever
 * the test does.
 */
async function listen(answer: (path: string) => { status: number; body: unknown } | null) {
	const server: Server = createServer((request, response) => {
		const given = answer(new URL(request.url ?? '/', 'http://127.0.0.1').pathname);
		if (given === null) {
			response.writeHead(404, { 'content-type': 'text/plain' });
			response.end('no\n');
			return;
		}
		const payload = JSON.stringify(given.body);
		response.writeHead(given.status, {
			'content-type': 'application/json',
			'content-length': Buffer.byteLength(payload)
		});
		response.end(payload);
	});
	const port = await new Promise<number>((resolve, reject) => {
		server.once('error', reject);
		server.listen(0, '127.0.0.1', () => {
			const address = server.address();
			resolve(typeof address === 'object' && address !== null ? address.port : 0);
		});
	});
	return {
		port,
		close: async () => {
			await new Promise<void>((resolve) => server.close(() => resolve()));
		}
	};
}

/** A port nothing is listening on, asked of the kernel the same way. */
async function freePort(): Promise<number> {
	const probe = await listen(() => null);
	await probe.close();
	return probe.port;
}

test.describe('the port a stale server would be on', () => {
	test('a free port is no news at all', async () => {
		const said = await askGuard(BUILD, '--reuse', await freePort());
		expect(said.code, said.stderr).toBe(0);
		expect(said.stderr).toBe('');
	});

	test('a server serving another build is refused, and named', async () => {
		// The stale `serve-like-gateway.mjs` of #185, exactly: this suite's own
		// server, still running, still perfectly healthy — and serving yesterday's
		// export.
		const decoy = await listen((path) =>
			path === SERVING_PATH
				? {
						status: 200,
						body: {
							server: 'serve-like-gateway',
							build: 'a1b2c3d4a1b2c3d4',
							root: '/somewhere/else/build',
							pid: process.pid,
							startedAt: '2026-09-18T00:26:00.000Z'
						}
					}
				: null
		);
		try {
			const said = await askGuard(BUILD, '--reuse', decoy.port);

			// The run stops. That is the whole point: it stops here, before a
			// single assertion, instead of producing thirty-six failures about
			// the code.
			expect(said.code).toBe(1);

			// And it says the four things somebody would otherwise spend a day
			// finding out: which port, what holds it, what that thing is serving,
			// and what this run expected.
			expect(said.stderr).toContain(`port ${decoy.port}`);
			expect(said.stderr).toContain(String(process.pid));
			expect(said.stderr).toContain('a1b2c3d4a1b2c3d4');
			expect(said.stderr).toContain(BUILD);
			expect(said.stderr).toContain('/somewhere/else/build');
			// Including the way out that does not involve killing somebody else's
			// run, because a hard stop for everybody is the other way to get this
			// wrong.
			expect(said.stderr).toContain('TWALK_TEST_PORT');

			// Not a 404, and not an assertion about a page: the failure is about
			// the port.
			expect(said.stderr).not.toContain('404');
		} finally {
			await decoy.close();
		}
	});

	test('a server that is not this suite at all is refused as what it is', async () => {
		// A dev server, another project's API, anything. It cannot say what it is
		// serving, so it is not this suite's server — which is the honest reading
		// and the one that does not depend on the other thing being well behaved.
		const decoy = await listen(() => null);
		try {
			const said = await askGuard(BUILD, '--reuse', decoy.port);
			expect(said.code).toBe(1);
			expect(said.stderr).toContain(`port ${decoy.port}`);
			expect(said.stderr).toContain(SERVING_PATH);
			expect(said.stderr).toContain("not this suite's server");
		} finally {
			await decoy.close();
		}
	});

	test('a server serving this very build is adopted, and the run says so', async () => {
		// The case that keeps this from being a hard stop: `npm run test:e2e`
		// twice over one build. The server proves it is serving the bytes under
		// test, so adopting it is a fact rather than a hope.
		const decoy = await listen((path) =>
			path === SERVING_PATH
				? {
						status: 200,
						body: { server: 'serve-like-gateway', build: BUILD, root: 'build', pid: 1 }
					}
				: null
		);
		try {
			const reusing = await askGuard(BUILD, '--reuse', decoy.port);
			expect(reusing.code, reusing.stderr).toBe(0);
			// Recorded, not silent: a run that met an already-running server should
			// say which build its assertions met.
			expect(reusing.stdout).toContain(`already serves build ${BUILD}`);

			// And on a real-stack run the same server is refused, because each
			// origin needs a Companion Gateway and a Synapse of its own behind it
			// and this one has none. Two different facts, two different answers.
			const exclusive = await askGuard(BUILD, '--exclusive', decoy.port);
			expect(exclusive.code).toBe(1);
			expect(exclusive.stderr).toContain('Gateway');
		} finally {
			await decoy.close();
		}
	});

	test('no build means no server can be proved, and the suite says which command', async () => {
		const said = await askGuard('-', '--reuse', await freePort());
		expect(said.code).toBe(1);
		expect(said.stderr).toContain('npm run build');
	});
});
