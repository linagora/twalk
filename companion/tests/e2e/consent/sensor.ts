// A **real Sensor**, running beside the test stack, because the label on a
// message is the only evidence this ticket accepts.
//
// # Why this exists
//
// #170's keystone criterion is: *a contact appears as awaiting a decision, is
// granted, and a **subsequent** message is labelled `granted` on the bus —
// asserted at the bus, not in the screen's own state.* Every other Companion
// journey publishes its own events onto the bus (`tests/e2e/dashboard/bus.ts`,
// `tests/e2e/approvals/harness.ts`), which is right for them: what those
// screens must be held to is what they do when an event exists.
//
// It is not right here. The consent label is stamped **by the Sensor at
// publication**, from a cache it fills from the Gateway's snapshot and then
// keeps in step by following `consent.state.changed` on the bus (ADR 0010). A
// test that published the label itself would be asserting its own string: the
// one thing that could be broken — that the decision this screen wrote actually
// reaches the process that labels messages — is the thing it would not test.
//
// So this starts the real binary. Nothing here is stubbed: the Sensor logs in
// to the real Synapse, joins a real room on a real invitation, reads real
// messages, and publishes to the real JetStream the real Gateway's outbox
// publishes the decision to.
//
// # No Companion Gateway seam, on purpose
//
// `SENSOR_GATEWAY_URL` and `SENSOR_GATEWAY_SERVICE_TOKEN` are left unset, which
// the Sensor announces at startup and handles: it starts with a cold consent
// cache — everything `pending` — and **still** creates the durable consumer on
// `consent.state.changed` and applies what arrives (`sensor/src/main.rs`,
// `bring_up_consent`). That is exactly the path under test, and it is stricter
// than configuring the seam would be: the `granted` label cannot have come from
// a snapshot read at startup, because there was none. It can only have come
// from the decision the browser took, through the Gateway's outbox, onto the
// bus, into the Sensor's cache.
//
// # What this costs, and the one thing it does not tolerate
//
// A `cargo build` of the Sensor (matrix-sdk and its crypto stack: minutes on a
// cold target directory), and a process that must be reaped. The durable
// consumer's name is a constant with no environment override
// (`consent::CONSENT_CONSUMER`), so **two Sensors on one bus would split the
// consent stream between them** — which is why `sensor/tests/harness` holds a
// lock around every test that starts one. This helper starts exactly one, and
// this Playwright project runs with a single worker.

import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..', '..', '..');

/** The account `tests/harness/scripts/provision-bots.sh` creates for the Sensor. */
export const SENSOR_LOCALPART = 'sensor';

export interface RunningSensor {
	userId: string;
	/** Everything the process has said, for a failure message worth reading. */
	readonly log: string[];
	stop(): Promise<void>;
}

export interface SensorOptions {
	synapseUrl: string;
	serverName: string;
	natsPort: number;
	/** Whose invitation the Sensor accepts. Nothing is observed by default. */
	allowedInviters: string[];
}

/**
 * Builds and starts the Sensor, and waits until it is actually running.
 *
 * `sensor running` is the repo's readiness marker and the reason is recorded in
 * `sensor/tests/observability.rs`: the JetStream stream persists across runs, so
 * its existence says nothing about *this* process. The line is emitted right
 * before the sync loop starts.
 */
export async function startSensor(options: SensorOptions): Promise<RunningSensor> {
	const binary = buildSensor();
	const stateDir = await mkdtemp(join(tmpdir(), 'twalk-companion-sensor-'));
	const userId = `@${SENSOR_LOCALPART}:${options.serverName}`;
	const log: string[] = [];

	const sensor: ChildProcess = spawn(binary, [], {
		env: {
			...process.env,
			SENSOR_HOMESERVER: options.synapseUrl,
			SENSOR_USER_ID: userId,
			SENSOR_PASSWORD: `test-only-password-${SENSOR_LOCALPART}`,
			SENSOR_NATS_URL: `nats://127.0.0.1:${options.natsPort}`,
			SENSOR_ALLOWED_INVITERS: options.allowedInviters.join(','),
			SENSOR_STATE_DIR: stateDir,
			SENSOR_LOG_LEVEL: process.env.SENSOR_LOG_LEVEL ?? 'info,twalk_sensor=debug'
			// SENSOR_GATEWAY_URL / SENSOR_GATEWAY_SERVICE_TOKEN: deliberately
			// absent — see this file's header.
			//
			// SENSOR_OWNER: deliberately absent too. With no owner configured
			// the Sensor publishes *every* sender's message through the inbound
			// door, including this deployment's owner — which is the world
			// before #109, and therefore the sharpest available probe of whether
			// the Gateway keeps the owner out of the list of people to decide
			// about (ADR 0018, ADR 0021, #149).
		},
		stdio: ['ignore', 'pipe', 'pipe']
	});

	const collect = (chunk: Buffer) => {
		for (const line of chunk.toString('utf8').split('\n')) {
			if (line.trim() !== '') {
				log.push(line);
			}
		}
	};
	sensor.stdout?.on('data', collect);
	sensor.stderr?.on('data', collect);

	const stop = async () => {
		sensor.kill('SIGTERM');
		await new Promise<void>((resolve) => {
			if (sensor.exitCode !== null || sensor.signalCode !== null) {
				resolve();
				return;
			}
			const give = setTimeout(() => {
				sensor.kill('SIGKILL');
				resolve();
			}, 10_000);
			sensor.once('exit', () => {
				clearTimeout(give);
				resolve();
			});
		});
		await rm(stateDir, { recursive: true, force: true }).catch(() => {});
	};

	try {
		await waitForLine(log, 'sensor running', 120_000, sensor);
	} catch (error) {
		await stop();
		throw error;
	}

	return { userId, log, stop };
}

/**
 * Waits for a line the Sensor has said.
 *
 * Fails with the whole log rather than a timeout on its own: a Sensor that
 * refused to start says exactly why on its first three lines, and a test that
 * hid that would send somebody to read this file instead.
 */
export async function waitForLine(
	log: readonly string[],
	needle: string,
	timeoutMs: number,
	process?: ChildProcess
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	for (;;) {
		if (log.some((line) => line.includes(needle))) {
			return;
		}
		if (process !== undefined && process.exitCode !== null) {
			throw new Error(
				`the Sensor exited with ${process.exitCode} before saying "${needle}":\n${log.join('\n')}`
			);
		}
		if (Date.now() > deadline) {
			throw new Error(
				`the Sensor did not say "${needle}" within ${timeoutMs} ms:\n${log.join('\n')}`
			);
		}
		await new Promise((resolve) => setTimeout(resolve, 200));
	}
}

function buildSensor(): string {
	const crate = join(repo, 'sensor');
	const result = spawnSync('cargo', ['build', '--bin', 'twalk-sensor'], {
		cwd: crate,
		stdio: 'inherit',
		timeout: 1_800_000
	});
	if (result.status !== 0) {
		throw new Error('cargo build failed for the Sensor');
	}
	return join(crate, 'target', 'debug', 'twalk-sensor');
}
