// Reading the bus from a Playwright spec.
//
// # Why this exists at all
//
// Ticket #69's first journey is "activate the assistant and see the consent
// event reach the **bus**". Asserting the screen, or even the Gateway's own
// `GET /api/consent/state`, would prove that the Companion asked and the
// Gateway wrote — not that the decision left the deployment as a
// `consent.state.changed.v1` event, which is the only form in which Hermes
// will ever learn that the persona is active (ADR 0013). The whole point of
// activation-as-a-consent-decision is that one event; a test that never looks
// at it is testing the half that cannot be wrong.
//
// # Why forty lines of protocol instead of a client library
//
// The NATS wire protocol for *subscribing* is text: read `INFO`, send
// `CONNECT` and `SUB`, then parse `MSG` frames. Adding a NATS client to the
// Companion's `package.json` would put a dependency in the shipped app's
// manifest for the sake of one test file, and the Companion is a browser app
// that must never speak to NATS. Declaring `headers: false` in `CONNECT` also
// makes the server strip the outbox's `Nats-Msg-Id` header for us, so every
// frame is a plain `MSG` and the payload is the CloudEvent, unchanged.
//
// A **core** subscription is enough even though the Gateway publishes into
// JetStream: a JetStream publish is an ordinary publish to a subject a stream
// happens to capture. Subscribe before the action, and the event arrives.

import { createConnection, type Socket } from 'node:net';

/** The subject the Gateway's outbox publishes consent decisions to. */
export const CONSENT_SUBJECT = 'twalk.consent.state.changed.v1';

export interface BusMessage {
	subject: string;
	/** The CloudEvent envelope, parsed. */
	event: {
		id: string;
		type: string;
		source: string;
		subject: string;
		data: {
			subject: { type: string; id: string };
			old_state: string;
			new_state: string;
			scope: { networks: string[] };
			actor: string;
		};
	};
}

/**
 * A live core subscription, collecting every message on `subject` from the
 * moment [`watchBus`] resolves.
 */
export interface BusWatch {
	/** Everything seen so far, oldest first. */
	readonly seen: BusMessage[];
	/** Waits for the first message matching `predicate`, or throws. */
	waitFor(predicate: (message: BusMessage) => boolean, timeoutMs?: number): Promise<BusMessage>;
	close(): void;
}

export async function watchBus(port: number, subject: string): Promise<BusWatch> {
	const socket: Socket = createConnection({ host: '127.0.0.1', port });
	socket.setEncoding('utf8');
	const seen: BusMessage[] = [];
	let buffer = '';
	/** Set while a `MSG` header has been read and its payload has not. */
	let pending: { subject: string; bytes: number } | null = null;

	socket.on('data', (chunk: string) => {
		buffer += chunk;
		for (;;) {
			if (pending !== null) {
				// The payload is `bytes` of UTF-8 followed by CRLF. The
				// CloudEvent is ASCII-safe JSON, so counting characters is
				// counting bytes here; anything else would need a Buffer.
				if (buffer.length < pending.bytes + 2) {
					return;
				}
				const payload = buffer.slice(0, pending.bytes);
				buffer = buffer.slice(pending.bytes + 2);
				try {
					seen.push({ subject: pending.subject, event: JSON.parse(payload) });
				} catch {
					// Not a CloudEvent: nothing on this subject should be, but
					// a malformed frame must not kill the socket.
				}
				pending = null;
				continue;
			}
			const end = buffer.indexOf('\r\n');
			if (end === -1) {
				return;
			}
			const line = buffer.slice(0, end);
			buffer = buffer.slice(end + 2);
			if (line.startsWith('PING')) {
				socket.write('PONG\r\n');
				continue;
			}
			if (line.startsWith('MSG ')) {
				// MSG <subject> <sid> [reply-to] <#bytes>
				const parts = line.split(' ').filter((part) => part.length > 0);
				pending = { subject: parts[1], bytes: Number(parts[parts.length - 1]) };
			}
		}
	});

	await new Promise<void>((resolve, reject) => {
		socket.once('error', reject);
		socket.once('connect', () => resolve());
	});

	socket.write(
		`CONNECT ${JSON.stringify({
			verbose: false,
			pedantic: false,
			tls_required: false,
			name: 'twalk-companion-e2e',
			lang: 'nodejs',
			version: '0.0.0',
			protocol: 1,
			headers: false
		})}\r\n`
	);
	socket.write(`SUB ${subject} 1\r\n`);
	// `PING`/`PONG` is the handshake's acknowledgement: once the server has
	// answered, the subscription is registered and a publish that happens
	// afterwards cannot be missed.
	await new Promise<void>((resolve, reject) => {
		const deadline = setTimeout(() => reject(new Error('NATS did not answer PING')), 10_000);
		const onData = (chunk: string) => {
			if (chunk.includes('PONG')) {
				clearTimeout(deadline);
				socket.off('data', onData);
				resolve();
			}
		};
		socket.on('data', onData);
		socket.write('PING\r\n');
	});

	return {
		seen,
		async waitFor(predicate, timeoutMs = 20_000) {
			const deadline = Date.now() + timeoutMs;
			for (;;) {
				const hit = seen.find(predicate);
				if (hit !== undefined) {
					return hit;
				}
				if (Date.now() > deadline) {
					throw new Error(
						`no bus message matched within ${timeoutMs} ms; saw ${JSON.stringify(
							seen.map((message) => message.event.data)
						)}`
					);
				}
				await new Promise((resolve) => setTimeout(resolve, 100));
			}
		},
		close() {
			socket.destroy();
		}
	};
}

/** Whether a message is a decision about this persona. */
export function aboutPersona(persona: string) {
	return (message: BusMessage) =>
		message.event.data.subject.type === 'persona' && message.event.data.subject.id === persona;
}

/** The subject the Sensor publishes observed messages to. */
export const INBOUND_SUBJECT = 'twalk.inbound.message.received.v1';

/**
 * Publishes one event, and waits for the server to have taken it.
 *
 * This is how a spec makes a contact *pending*: the Gateway's projection (#54)
 * is a durable consumer on the inbound subject, so one message from an unknown
 * sender is one more person waiting for a decision. Publishing from the test
 * rather than running a Sensor is deliberate — the Sensor's own suites prove
 * it produces these events; what the dashboard has to be held to is what it
 * does when one arrives.
 *
 * A JetStream publish is an ordinary publish to a subject a stream captures,
 * so `PUB` is all this needs. The round trip through `PING`/`PONG` is what
 * makes it safe to assert on the consequence straight afterwards.
 */
export async function publish(port: number, subject: string, event: unknown): Promise<void> {
	const socket: Socket = createConnection({ host: '127.0.0.1', port });
	socket.setEncoding('utf8');
	try {
		await new Promise<void>((resolve, reject) => {
			socket.once('error', reject);
			socket.once('connect', () => resolve());
		});
		socket.write(
			`CONNECT ${JSON.stringify({
				verbose: false,
				pedantic: false,
				tls_required: false,
				name: 'twalk-companion-e2e-publisher',
				lang: 'nodejs',
				version: '0.0.0',
				protocol: 1,
				headers: false
			})}\r\n`
		);
		const payload = JSON.stringify(event);
		socket.write(`PUB ${subject} ${Buffer.byteLength(payload)}\r\n${payload}\r\n`);
		await new Promise<void>((resolve, reject) => {
			const deadline = setTimeout(() => reject(new Error('NATS did not answer PING')), 10_000);
			socket.on('data', (chunk: string) => {
				if (chunk.includes('-ERR')) {
					clearTimeout(deadline);
					reject(new Error(`NATS refused the publish: ${chunk.trim()}`));
				}
				if (chunk.includes('PONG')) {
					clearTimeout(deadline);
					resolve();
				}
			});
			socket.write('PING\r\n');
		});
	} finally {
		socket.destroy();
	}
}
