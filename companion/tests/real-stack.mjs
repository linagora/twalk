// The real stack behind the test server: a real Synapse and a real Companion
// Gateway, for the journey Playwright cannot fake.
//
// Spec #65 makes Playwright authoritative "against the real stack: the built
// export served by the real Gateway, a real Synapse". The cryptographic
// bootstrap of ADR 0014 is why that is not negotiable — cross-signing, secret
// storage and a key backup are round trips to a homeserver, and a stub that
// answered them would be asserting its own behaviour.
//
// What this brings up, and what talks to what:
//
//   docker compose (tests/harness/compose.test.yaml)   the shared test stack
//     └── Synapse on TWALK_TEST_SYNAPSE_PORT, server_name `test.twalk`
//   the Gateway binary, built by cargo, on a free port
//     ├── GATEWAY_HOMESERVER_URL → that Synapse
//     └── its own SQLite state in a fresh directory, per run
//   tests/serve-like-gateway.mjs                       the origin the browser
//     ├── serves `build/` with the Gateway's own resolution order
//     └── proxies `/api/*` to the Gateway binary
//
// Same origin for the files and the API is not a convenience either: the
// device token is an `HttpOnly` cookie (ADR 0011), and a cross-origin test
// would prove a cookie flow nobody ships.
//
// **One account, ever.** The Gateway refuses a second registration, and
// Synapse's data outlives a `docker compose up`. So each run invents its own
// owner localpart and gives the Gateway an empty state directory: the journey
// is repeatable without tearing the stack down, which matters because the
// stack is shared with the Rust suites.

import { spawn, spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

const here = fileURLToPath(new URL('.', import.meta.url));
const repo = resolve(here, '..', '..');

const PROJECT = process.env.TWALK_TEST_STACK ?? 'twalk-test-c67';
const SYNAPSE_PORT = Number(process.env.TWALK_TEST_SYNAPSE_PORT ?? 19148);
const NATS_PORT = Number(process.env.TWALK_TEST_NATS_PORT ?? 15348);
const SERVER_NAME = 'test.twalk';
const REGISTRATION_SECRET = 'test-only-registration-shared-secret';

/** Where the specs read what this run created. */
export const STACK_FILE = join(here, '.real-stack.json');

/**
 * Brings the stack up and returns what the tests need to know. Idempotent
 * across runs: `docker compose up` on an existing project is a no-op, and the
 * Gateway is a fresh process each time.
 */
export async function startRealStack() {
	const synapseUrl = `http://127.0.0.1:${SYNAPSE_PORT}`;

	await composeUp();
	await waitFor(`${synapseUrl}/health`, 'Synapse', 120_000);

	// A localpart nobody has used, so the one-account rule is a rule about
	// this run rather than about this machine's history.
	const owner = `owner${Date.now().toString(36)}${Math.floor(Math.random() * 1000)}`;
	const stateDir = await mkdtemp(join(tmpdir(), 'twalk-companion-gateway-'));
	const gatewayPort = await freePort();

	const binary = buildGateway();
	const gateway = spawn(binary, [], {
		env: {
			...process.env,
			GATEWAY_LISTEN: `127.0.0.1:${gatewayPort}`,
			// Required, and never read through this path: the browser is
			// served by `serve-like-gateway.mjs`, which is the point.
			GATEWAY_STATIC_DIR: join(here, '..', 'build'),
			GATEWAY_STATE_DIR: stateDir,
			GATEWAY_OWNER: `@${owner}:${SERVER_NAME}`,
			GATEWAY_HOMESERVER_URL: synapseUrl,
			GATEWAY_HOMESERVER_FEDERATION_URL: synapseUrl,
			GATEWAY_REGISTRATION_SHARED_SECRET: REGISTRATION_SECRET,
			GATEWAY_SENSOR_USER_ID: `@sensor:${SERVER_NAME}`,
			GATEWAY_LOG_LEVEL: process.env.GATEWAY_LOG_LEVEL ?? 'info'
		},
		stdio: ['ignore', 'inherit', 'inherit']
	});
	gateway.on('exit', (code) => {
		if (code !== 0 && code !== null) {
			console.error(`the Companion Gateway exited with ${code}`);
		}
	});

	const origin = `http://127.0.0.1:${gatewayPort}`;
	await waitFor(`${origin}/health`, 'the Companion Gateway', 30_000);

	const info = {
		owner,
		ownerId: `@${owner}:${SERVER_NAME}`,
		serverName: SERVER_NAME,
		/** What the user types on screen 1. Loopback, so the browser can reach it. */
		domain: `127.0.0.1:${SYNAPSE_PORT}`,
		synapseUrl,
		gatewayOrigin: origin,
		natsPort: NATS_PORT
	};
	await writeFile(STACK_FILE, `${JSON.stringify(info, null, '\t')}\n`);

	const stop = async () => {
		gateway.kill('SIGTERM');
		await rm(stateDir, { recursive: true, force: true }).catch(() => {});
	};
	process.on('exit', () => gateway.kill('SIGTERM'));

	return { ...info, stop };
}

async function composeUp() {
	const result = spawnSync(
		'docker',
		[
			'compose',
			'-p',
			PROJECT,
			'-f',
			join(repo, 'tests', 'harness', 'compose.test.yaml'),
			'up',
			'-d',
			'--wait'
		],
		{
			cwd: join(repo, 'tests', 'harness'),
			env: {
				...process.env,
				TWALK_TEST_SYNAPSE_PORT: String(SYNAPSE_PORT),
				TWALK_TEST_NATS_PORT: String(NATS_PORT)
			},
			stdio: 'inherit',
			timeout: 300_000
		}
	);
	if (result.status !== 0) {
		throw new Error(`docker compose up failed for project ${PROJECT}`);
	}
}

/**
 * `cargo build`, because the binary is the thing under test on the Gateway's
 * side and a stale one would be a lie. Cargo is incremental, so this is a
 * no-op once warm.
 */
function buildGateway() {
	const crate = join(repo, 'companion-gateway');
	const result = spawnSync('cargo', ['build', '--bin', 'twalk-companion-gateway'], {
		cwd: crate,
		stdio: 'inherit',
		timeout: 900_000
	});
	if (result.status !== 0) {
		throw new Error('cargo build failed for the Companion Gateway');
	}
	return join(crate, 'target', 'debug', 'twalk-companion-gateway');
}

async function waitFor(url, what, timeoutMs) {
	const deadline = Date.now() + timeoutMs;
	for (;;) {
		try {
			const response = await fetch(url);
			if (response.ok) {
				return;
			}
		} catch {
			// Not up yet.
		}
		if (Date.now() > deadline) {
			throw new Error(`${what} did not answer ${url} within ${timeoutMs} ms`);
		}
		await new Promise((resolve) => setTimeout(resolve, 500));
	}
}

/** Asks the kernel, the way the Gateway's own Rust tests do. */
async function freePort() {
	return await new Promise((resolvePort, reject) => {
		const probe = createServer();
		probe.on('error', reject);
		probe.listen(0, '127.0.0.1', () => {
			const { port } = probe.address();
			probe.close(() => resolvePort(port));
		});
	});
}
