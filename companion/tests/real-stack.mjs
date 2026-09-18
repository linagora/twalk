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
/** The same, for the bridge journeys' own Gateway (ticket #68). */
export const BRIDGE_STACK_FILE = join(here, '.bridge-stack.json');

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

/**
 * The same stack, for the **bridge** journeys of ticket #68: screens 3, 3a–3d.
 *
 * Everything here is real except the bridge, which is
 * [`./stub-bridge.mjs`] — a real mautrix-whatsapp needs a live WhatsApp
 * account and a human with a phone (spec #47), so it can never be in a suite.
 * What the suite does prove is the contract: the Gateway holds the bridge's
 * blocking step, the browser polls, and the code drawn on screen is the
 * payload the bridge handed over.
 *
 * ## Why a second Gateway rather than the one above
 *
 * [`startRealStack`] gives its Gateway an owner whose account does **not**
 * exist, because that is what the bootstrap journey walks the user through
 * creating, and the Gateway creates this deployment's one account and refuses
 * a second. A bridge login is the opposite: it needs a signed-in device, so it
 * needs an owner whose account already exists. One Gateway cannot be both, so
 * this one is configured with `bot_alpha` — provisioned on the shared test
 * stack by `tests/harness/scripts/provision-bots.sh`, with the password scheme
 * every Twalk suite uses.
 *
 * Same compose stack, same Gateway binary, same proxy in
 * `serve-like-gateway.mjs`: only the owner and the bridges differ.
 */
export async function startBridgeStack() {
	const synapseUrl = `http://127.0.0.1:${SYNAPSE_PORT}`;

	// The two stacks share one compose project and Playwright starts their
	// servers at the same time, so this one brings the stack up only when
	// nothing answers: two concurrent `docker compose up` on one project is a
	// race, and the other server is already running one.
	if (!(await answers(`${synapseUrl}/health`))) {
		await composeUp();
	}
	await waitFor(`${synapseUrl}/health`, 'Synapse', 300_000);
	provisionBots();

	const { startStubBridge, STUB_PROVISIONING_SECRET } = await import('./stub-bridge.mjs');
	const stub = await startStubBridge(
		BRIDGES.map((bridge) => bridge.bridgeId),
		{
			// The stub's control surface is where a test asks for a session,
			// because it is already proxied onto the browser's own origin. The
			// homeserver is this file's business, not the stub's.
			signIn: async (deviceName) => signInOwner(synapseUrl, gatewayOrigin, deviceName),
			matrixUser: async (localpart) => {
				const user = await matrixLogin(synapseUrl, localpart ?? OWNER_LOCALPART);
				return {
					user_id: user.user_id,
					access_token: user.access_token,
					homeserver: synapseUrl,
					sensor: `@sensor:${SERVER_NAME}`
				};
			}
		}
	);

	const bridgeEnvironment = {};
	for (const { bridgeId, network } of BRIDGES) {
		const slug = bridgeId.toUpperCase().replace(/[^A-Z0-9]/gu, '_');
		bridgeEnvironment[`GATEWAY_BRIDGE_${slug}_URL`] = stub.bridgeUrl(bridgeId);
		bridgeEnvironment[`GATEWAY_BRIDGE_${slug}_PROVISIONING_SECRET`] = STUB_PROVISIONING_SECRET;
		bridgeEnvironment[`GATEWAY_BRIDGE_${slug}_NETWORK`] = network;
	}

	const stateDir = await mkdtemp(join(tmpdir(), 'twalk-companion-bridges-'));
	const gatewayPort = await freePort();
	const gateway = spawn(buildGateway(), [], {
		env: {
			...process.env,
			GATEWAY_LISTEN: `127.0.0.1:${gatewayPort}`,
			GATEWAY_STATIC_DIR: join(here, '..', 'build'),
			GATEWAY_STATE_DIR: stateDir,
			GATEWAY_OWNER: `@${OWNER_LOCALPART}:${SERVER_NAME}`,
			GATEWAY_HOMESERVER_URL: synapseUrl,
			GATEWAY_HOMESERVER_FEDERATION_URL: synapseUrl,
			GATEWAY_REGISTRATION_SHARED_SECRET: REGISTRATION_SECRET,
			GATEWAY_SENSOR_USER_ID: `@sensor:${SERVER_NAME}`,
			// The consent endpoints answer `503 consent_not_configured`
			// without a bus, and ticket #69 writes a consent decision on the
			// `assistant` persona — which its journey then reads back off
			// NATS, because "the UI said so" is not evidence that the
			// decision left the Gateway (ADR 0013).
			GATEWAY_NATS_URL: `nats://127.0.0.1:${NATS_PORT}`,
			GATEWAY_BRIDGES: BRIDGES.map((bridge) => bridge.bridgeId).join(','),
			...bridgeEnvironment,
			GATEWAY_LOG_LEVEL: process.env.GATEWAY_LOG_LEVEL ?? 'info'
		},
		stdio: ['ignore', 'inherit', 'inherit']
	});
	gateway.on('exit', (code) => {
		if (code !== 0 && code !== null) {
			console.error(`the Companion Gateway (bridges) exited with ${code}`);
		}
	});

	const gatewayOrigin = `http://127.0.0.1:${gatewayPort}`;
	await waitFor(`${gatewayOrigin}/health`, 'the Companion Gateway', 30_000);

	const info = {
		owner: OWNER_LOCALPART,
		ownerId: `@${OWNER_LOCALPART}:${SERVER_NAME}`,
		serverName: SERVER_NAME,
		domain: `127.0.0.1:${SYNAPSE_PORT}`,
		synapseUrl,
		gatewayOrigin,
		stubOrigin: stub.origin,
		bridges: BRIDGES,
		natsPort: NATS_PORT
	};
	await writeFile(BRIDGE_STACK_FILE, `${JSON.stringify(info, null, '\t')}\n`);

	process.on('exit', () => gateway.kill('SIGTERM'));

	return {
		...info,
		stop: async () => {
			gateway.kill('SIGTERM');
			await stub.close();
			await rm(stateDir, { recursive: true, force: true }).catch(() => {});
		}
	};
}

/**
 * The bridges the networks suite configures, and the network each one serves.
 * Three, because screen 3's picker is a join of the catalogue with this list
 * and a screen that only ever saw one bridge would not prove it.
 */
const BRIDGES = [
	{ bridgeId: 'mautrix-whatsapp', network: 'whatsapp' },
	{ bridgeId: 'mautrix-signal', network: 'signal' },
	{ bridgeId: 'mautrix-gmessages', network: 'sms' }
];

/** The shared stack's owner bot for the bridge journeys, and its password scheme. */
const OWNER_LOCALPART = 'bot_alpha';

/** Idempotent: an account that exists is skipped. */
function provisionBots() {
	const result = spawnSync(join(repo, 'tests', 'harness', 'scripts', 'provision-bots.sh'), [], {
		env: { ...process.env, TWALK_TEST_STACK: PROJECT },
		stdio: 'inherit',
		timeout: 300_000
	});
	if (result.status !== 0) {
		throw new Error('provision-bots.sh failed');
	}
}

/**
 * A Matrix account on the test stack, standing in for the user's own browser.
 * Its access token stays on this side of the seam: the Gateway is handed an
 * OpenID token and never the token that minted it (ADR 0011).
 */
async function matrixLogin(synapseUrl, localpart) {
	const answer = await fetch(`${synapseUrl}/_matrix/client/v3/login`, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({
			type: 'm.login.password',
			identifier: { type: 'm.id.user', user: localpart },
			password: `test-only-password-${localpart}`
		})
	});
	if (!answer.ok) {
		throw new Error(`the homeserver refused to log ${localpart} in: ${answer.status}`);
	}
	return answer.json();
}

/**
 * One signed-in device, the way the Companion gets one: a Matrix OpenID token
 * from the homeserver, exchanged at the Gateway for a device token.
 *
 * The token comes back to the test rather than as a cookie the browser
 * already holds, because the Gateway sets it `HttpOnly` and a spec cannot read
 * it back out of a page. What reaches the Gateway is identical either way.
 */
async function signInOwner(synapseUrl, gatewayOrigin, deviceName) {
	const user = await matrixLogin(synapseUrl, OWNER_LOCALPART);
	const token = await fetch(
		`${synapseUrl}/_matrix/client/v3/user/${encodeURIComponent(user.user_id)}/openid/request_token`,
		{
			method: 'POST',
			headers: { 'content-type': 'application/json', authorization: `Bearer ${user.access_token}` },
			body: '{}'
		}
	);
	if (!token.ok) {
		throw new Error(`the homeserver refused an OpenID token: ${token.status}`);
	}
	const answer = await fetch(`${gatewayOrigin}/api/session`, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({ matrix_openid_token: await token.json(), device_name: deviceName })
	});
	const session = await answer.json();
	const cookie = answer.headers
		.getSetCookie()
		.map((value) => value.split(';')[0])
		.find((pair) => pair.startsWith('twalk_device='));
	if (cookie === undefined) {
		throw new Error(`the sign-in set no device cookie: ${answer.status}`);
	}
	return { device_token: cookie.slice('twalk_device='.length), session };
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

/** Whether something already answers, without waiting for it to start. */
async function answers(url) {
	try {
		return (await fetch(url)).ok;
	} catch {
		return false;
	}
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
