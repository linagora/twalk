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
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
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

/**
 * The durable JetStream consumer the pending-contact projection reads from
 * (#54), named per Gateway **and per run**.
 *
 * Per Gateway because two Gateways sharing one durable name would split the
 * stream between them and leave each with half a list — the Gateway's own
 * configuration warns about exactly that. Per run because the durable consumer
 * *is* the cursor: each run gives its Gateway an empty state directory, so a
 * consumer that had already acked the stream's history would rebuild that
 * empty store from nothing and report an empty list. A fresh name replays the
 * history into the fresh store, which is what makes the pending count a
 * property of the bus rather than of how many times the suite has run.
 *
 * The consumers it leaves behind die with the stack (`docker compose down -v`).
 */
function inboundConsumer(which) {
	return `companion-e2e-${which}-${RUN}`;
}

/** This run's tag, for anything that must not be shared with the last one. */
const RUN = `${Date.now().toString(36)}${Math.floor(Math.random() * 1000)}`;

/** Where the specs read what this run created. */
export const STACK_FILE = join(here, '.real-stack.json');
/** The same, for the bridge journeys' own Gateway (ticket #68). */
export const BRIDGE_STACK_FILE = join(here, '.bridge-stack.json');
/** And for the session journeys' own Gateway (ticket #111). */
export const SESSION_STACK_FILE = join(here, '.session-stack.json');

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
			// The consent endpoints answer `503 consent_not_configured`
			// without a bus, and the journey ends by activating the assistant
			// — which is a consent decision and nothing else (ADR 0013).
			GATEWAY_NATS_URL: `nats://127.0.0.1:${NATS_PORT}`,
			GATEWAY_INBOUND_CONSUMER: inboundConsumer('bootstrap'),
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
	await withLock('provision-bots', () => provisionBots());

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

	// The portal register's own accounts, created through the appservice the
	// way a bridge creates its bot and its ghosts. Registering them is not
	// optional: Synapse refuses to let an appservice act as a user it has not
	// registered, even one its namespace covers.
	await withLock('portal-appservice', async () => {
		await registerAppserviceUser(synapseUrl, PORTAL_BOT_LOCALPART);
		await registerAppserviceUser(synapseUrl, PORTAL_GHOST_LOCALPART);
		for (let at = 0; at < CROWDED_MEMBERS; at += 1) {
			await registerAppserviceUser(synapseUrl, `${PORTAL_GHOST_LOCALPART}_${at}`);
		}
	});
	const portalBot = `@${PORTAL_BOT_LOCALPART}:${SERVER_NAME}`;

	const bridgeEnvironment = {};
	for (const { bridgeId, network } of BRIDGES) {
		const slug = bridgeId.toUpperCase().replace(/[^A-Z0-9]/gu, '_');
		bridgeEnvironment[`GATEWAY_BRIDGE_${slug}_URL`] = stub.bridgeUrl(bridgeId);
		bridgeEnvironment[`GATEWAY_BRIDGE_${slug}_PROVISIONING_SECRET`] = STUB_PROVISIONING_SECRET;
		bridgeEnvironment[`GATEWAY_BRIDGE_${slug}_NETWORK`] = network;
	}
	// One bridge gets a portal register (#143, #171): the appservice token to
	// read with, and the bot to read **as**. The other two are left without,
	// which is the case the screen has to report rather than hide.
	const whatsappSlug = 'MAUTRIX_WHATSAPP';
	bridgeEnvironment[`GATEWAY_BRIDGE_${whatsappSlug}_AS_TOKEN`] = PORTALS_AS_TOKEN;
	bridgeEnvironment[`GATEWAY_BRIDGE_${whatsappSlug}_BOT_USER_ID`] = portalBot;

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
			GATEWAY_INBOUND_CONSUMER: inboundConsumer('bridges'),
			GATEWAY_BRIDGES: BRIDGES.map((bridge) => bridge.bridgeId).join(','),
			...bridgeEnvironment,
			GATEWAY_PORTAL_REFRESH_SECONDS: '0',
			// Not the default, on purpose: the chooser holds no threshold of its
			// own (#252), so the value it draws its crowds from has to be the
			// one this Gateway serves — and a screen asserting the served number
			// only proves it if the number is one it could not have guessed.
			GATEWAY_CROWD_THRESHOLD: String(CROWD_THRESHOLD),
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
		natsPort: NATS_PORT,
		/** What the portal journey needs to build real conversations (#143). */
		portals: {
			appserviceToken: PORTALS_AS_TOKEN,
			bot: portalBot,
			ghost: `@${PORTAL_GHOST_LOCALPART}:${SERVER_NAME}`,
			crowd: Array.from(
				{ length: CROWDED_MEMBERS },
				(_, at) => `@${PORTAL_GHOST_LOCALPART}_${at}:${SERVER_NAME}`
			),
			crowdThreshold: CROWD_THRESHOLD,
			sensorId: `@${SENSOR_LOCALPART}:${SERVER_NAME}`
		}
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
 * Registers one user through the appservice token, as a bridge does for its bot
 * and for each ghost. Idempotent: `M_USER_IN_USE` means a previous run did it,
 * and the stack outlives a run.
 */
async function registerAppserviceUser(synapseUrl, localpart) {
	const answer = await fetch(`${synapseUrl}/_matrix/client/v3/register`, {
		method: 'POST',
		headers: {
			'content-type': 'application/json',
			authorization: `Bearer ${PORTALS_AS_TOKEN}`
		},
		body: JSON.stringify({ type: 'm.login.application_service', username: localpart })
	});
	if (answer.ok) {
		return;
	}
	const body = await answer.text();
	if (body.includes('M_USER_IN_USE')) {
		return;
	}
	// Loud, and naming the cause: an unregistered appservice makes every portal
	// assertion fail as though the Gateway were asking the wrong account, which
	// is the defect under test wearing the harness's clothes.
	throw new Error(
		`the test Synapse would not register the appservice user @${localpart}: ${answer.status} ${body}\n` +
			'The registration is tests/harness/synapse/appservice-portals.yaml. If Synapse has ' +
			'never loaded it, recreate the container: docker compose -p ' +
			`${PROJECT} -f tests/harness/compose.test.yaml up -d --force-recreate synapse`
	);
}


/**
 * A third Gateway, for the **session** journeys of ticket #111: the refresh,
 * the central `401` repair, and the session-expired state.
 *
 * ## Why a Gateway of its own, and why its token lives five seconds
 *
 * The defect is a credential expiring under a working screen. Waiting fifteen
 * minutes for that in a browser test is not an option, and the ticket says so:
 * expire the token **at the Gateway** instead. `GATEWAY_DEVICE_TOKEN_TTL` is
 * the operator's knob for exactly that, and five seconds is a fifteen-minute
 * afternoon in miniature — the same code path, the same rotation, the same
 * `expires_in`.
 *
 * It cannot be the bridge journeys' Gateway because that TTL is deployment-wide:
 * every other spec on that origin would spend its life mid-expiry, and #68's
 * assertions would start measuring this ticket's behaviour instead of their own.
 * The refresh token keeps its default thirty days, so *device token dead,
 * refresh token alive* — the case the whole mechanism exists for — is simply
 * what this Gateway is, five seconds after any sign-in.
 *
 * The owner is `bot_alpha`, as the bridge stack's is: signing in needs an
 * account that already exists. The bridges are stubbed the same way, so that a
 * connected network can be connected here and still read as connected after a
 * session has died and been signed in again.
 */
export async function startSessionStack() {
	const synapseUrl = `http://127.0.0.1:${SYNAPSE_PORT}`;

	if (!(await answers(`${synapseUrl}/health`))) {
		await composeUp();
	}
	await waitFor(`${synapseUrl}/health`, 'Synapse', 300_000);
	await withLock('provision-bots', () => provisionBots());

	const { startStubBridge, STUB_PROVISIONING_SECRET } = await import('./stub-bridge.mjs');
	const stub = await startStubBridge(
		BRIDGES.map((bridge) => bridge.bridgeId),
		{
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

	const stateDir = await mkdtemp(join(tmpdir(), 'twalk-companion-session-'));
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
			GATEWAY_NATS_URL: `nats://127.0.0.1:${NATS_PORT}`,
			GATEWAY_INBOUND_CONSUMER: inboundConsumer('session'),
			GATEWAY_BRIDGES: BRIDGES.map((bridge) => bridge.bridgeId).join(','),
			...bridgeEnvironment,
			// The whole point of this origin. The default is 900, it is correct,
			// and #111 is explicit that raising it to hide the bug is the one
			// thing not to do — so here it is lowered instead, to make the same
			// expiry arrive in a test's lifetime.
			GATEWAY_DEVICE_TOKEN_TTL: String(SESSION_DEVICE_TOKEN_TTL_SECONDS),
			GATEWAY_LOG_LEVEL: process.env.GATEWAY_LOG_LEVEL ?? 'info'
		},
		stdio: ['ignore', 'inherit', 'inherit']
	});
	gateway.on('exit', (code) => {
		if (code !== 0 && code !== null) {
			console.error(`the Companion Gateway (session) exited with ${code}`);
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
		natsPort: NATS_PORT,
		deviceTokenTtlSeconds: SESSION_DEVICE_TOKEN_TTL_SECONDS
	};
	await writeFile(SESSION_STACK_FILE, `${JSON.stringify(info, null, '\t')}\n`);

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
 * How long a device token lives on the session origin.
 *
 * **Fifteen seconds, and the number is the fix for #186.** It was five, and five
 * was the race.
 *
 * What a test on this origin is measuring is the client's *headroom*: the
 * Companion renews a token with a fifth of its lifetime still in hand
 * (`refreshAfterSeconds` in `src/lib/session/refresh.ts`, `margin = expiresIn /
 * 5`, at least a second). That fraction is right for a deployment — a
 * fifteen-minute token is renewed after twelve minutes, three whole minutes of
 * slack — and it means the *absolute* headroom a test gets is a fifth of whatever
 * this origin is configured with. At five seconds that was **one second**. A
 * browser timer on a machine that is also compiling a Rust binary slips further
 * than that, and when it does the page holds a dead token while the specs are
 * asserting that it never does. The suite was racing the mechanism it exists to
 * observe, which is why it failed differently every time.
 *
 * Fifteen buys **three seconds** of headroom, stated as this suite's tolerance in
 * `tests/e2e/session/harness.ts` and asserted there rather than assumed. It costs
 * about forty-five seconds of wall clock across the four journeys, and it changes
 * nothing about what they prove: the same code path, the same rotation, the same
 * `expires_in`, the same expiry **arranged at the Gateway** rather than waited for
 * — which is what #111 asked for and what it refused was the fifteen real minutes.
 *
 * Two seconds was tried when this was written and was already too short: a
 * browser can spend that much fetching the app. That direction has not changed;
 * the mistake was reading "long enough to load the app" as the requirement, when
 * the requirement is "long enough that the client's own margin survives a loaded
 * host".
 */
const SESSION_DEVICE_TOKEN_TTL_SECONDS = 15;

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

/**
 * The test stack's appservice, and the bridge bot the portal register acts as
 * (ticket #171, and #143's chooser is what needs it).
 *
 * The registration is `tests/harness/synapse/appservice-portals.yaml`, and the
 * part of it that matters here is that its `sender_localpart` is an account in
 * **no rooms** — the shape a generated mautrix registration has. The account in
 * every portal room is this bot, and the Gateway reaches it by naming it in
 * `?user_id=`, which Synapse honours only for an appservice token. An ordinary
 * access token would make the whole mechanism untestable: the asker would be
 * the token's own user either way.
 */
const PORTALS_AS_TOKEN = 'test-only-portals-appservice-as-token';
/** In the registration's namespace (`@portalbot…`), and not its sender. */
const PORTAL_BOT_LOCALPART = 'portalbot_wa';
/** A network ghost, named the way mautrix names one, so the Sensor attributes
 *  its messages to WhatsApp exactly as it would in production. */
const PORTAL_GHOST_LOCALPART = 'portalbot_ghost_wa';
/**
 * The crowd threshold the portals Gateway is started with (`GATEWAY_CROWD_THRESHOLD`,
 * #252). Deliberately not the default 20, so a screen that shows it proves it
 * read the served value.
 */
const CROWD_THRESHOLD = 21;
/**
 * How many people the crowded group has.
 *
 * At or above the served threshold, because the criterion the ticket cares
 * most about is that a conversation covering a crowd cannot be ticked without
 * the number being read — and a browser test of that needs a room that really
 * does hold that many people. Twenty-two real accounts, joined for real: the
 * count on screen is the homeserver's own.
 */
const CROWDED_MEMBERS = 22;
/**
 * The Sensor's account on the shared stack (`provision-bots.sh`).
 *
 * The **process** is not started here. `tests/e2e/consent/sensor.ts` starts one
 * for the `consent` project and the `portals` project borrows the same helper,
 * because the Sensor's consent consumer is a durable with a constant name and
 * two Sensors on one bus would split the consent stream between them. What this
 * file publishes is only the account's Matrix ID, which is what a portal
 * journey asks the homeserver about.
 */
const SENSOR_LOCALPART = 'sensor';

/**
 * Runs `work` with nobody else in this checkout running it.
 *
 * Playwright starts every `webServer` at once, and since #111 there are three
 * of them, each orchestrating a Gateway on the one shared test stack. Bringing
 * the bots up is the step that does not tolerate company: two registrations of
 * the same localpart racing is a flake nobody would enjoy debugging. A
 * directory is the lock because creating one is atomic on every filesystem that
 * matters.
 */
async function withLock(name, work) {
	const path = join(tmpdir(), `twalk-companion-${name}.lock`);
	const deadline = Date.now() + 300_000;
	for (;;) {
		try {
			await mkdir(path);
			break;
		} catch (error) {
			if (error.code !== 'EEXIST') {
				throw error;
			}
			if (Date.now() > deadline) {
				// A lock this old belongs to a run that died. Take it.
				break;
			}
			await new Promise((resolve) => setTimeout(resolve, 250));
		}
	}
	try {
		return await work();
	} finally {
		await rm(path, { recursive: true, force: true }).catch(() => {});
	}
}

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
	const pairs = answer.headers.getSetCookie().map((value) => value.split(';')[0]);
	const cookie = pairs.find((pair) => pair.startsWith('twalk_device='));
	if (cookie === undefined) {
		throw new Error(`the sign-in set no device cookie: ${answer.status}`);
	}
	// The refresh token too, for the session journeys of ticket #111: a browser
	// holding only the device cookie can never refresh, so a test that set only
	// that one would be asserting a session the product does not issue.
	const refresh = pairs.find((pair) => pair.startsWith('twalk_refresh='));
	return {
		device_token: cookie.slice('twalk_device='.length),
		refresh_token: refresh === undefined ? null : refresh.slice('twalk_refresh='.length),
		session
	};
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
