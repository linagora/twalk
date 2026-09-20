// A stub mautrix bridge, in Node, for the Companion's Playwright journeys.
//
// # Why a stub, and why this one
//
// A real mautrix-whatsapp needs a live WhatsApp account and a human with a
// phone, so it can never be in a suite — spec #47 says so in as many words, and
// the Gateway's own suite reached the same conclusion
// (`companion-gateway/tests/harness/stub_bridge.rs`). What *can* be in a suite
// is the provisioning contract, and this file is that same stub transcribed
// into the language the Companion's tests are written in, so a browser journey
// can drive the bridge's side of a login: hold the blocking step, refresh the
// code, complete it, refuse it.
//
// It is deliberately the *same* contract, not a second one. `/_matrix/provision/v3`
// with a bearer secret, `display_and_wait` that does not answer until something
// releases it, `{"type": "qr", "data": "…"}` as the payload a real bridge hands
// over — the browser is what draws the code, here as in production.
//
// # Multi-tenant, because screen 3 is
//
// The picker shows one card per network and the Gateway configures one bridge
// per instance, so the tests need three at once (`whatsapp`, `signal`, `sms`).
// Each lives under `/bridges/<bridge_id>` with state of its own, which is what
// the Gateway's `GATEWAY_BRIDGE_<ID>_URL` points at.
//
// # The control surface
//
// The Rust stub is driven by method calls because it runs inside the test
// process. This one runs beside the browser, so the driving is HTTP:
// `/stub-control/<bridge_id>/…`, reachable from a Playwright test through the
// origin the Companion is served from (`tests/serve-like-gateway.mjs` proxies
// that prefix here, unchanged). It is a test fixture and has no authentication
// — it listens on loopback and exists only while the suite runs.

import { createServer } from 'node:http';

/** The provisioning secret the Gateway is configured with, as mautrix requires (16+ characters). */
export const STUB_PROVISIONING_SECRET = 'test-only-stub-bridge-provisioning-secret';

const QR_FLOW = 'qr';
const COOKIES_FLOW = 'cookies';
const PHONE_FLOW = 'phone';
const QR_STEP = 'fi.mau.stub.login.qr';
const COOKIES_STEP = 'fi.mau.stub.login.cookies';
const EMOJI_STEP = 'fi.mau.stub.login.emoji';
const PHONE_STEP = 'fi.mau.stub.login.phone';

function freshBridge() {
	return {
		/** Live login processes, by id. */
		processes: new Map(),
		/** Queued answers for the held blocking step. */
		releases: [],
		/**
		 * Steps a test has queued for the next submits, in order.
		 *
		 * Without this the stub could not serve two `user_input` steps in a
		 * row — every submit that was not blocking or cookies completed the
		 * login — so a password-then-code sequence, a step re-issued after a
		 * refusal and a field type the Companion cannot draw had no fixture
		 * able to express them (ADR 0030).
		 */
		nextSteps: [],
		/** Canned refusals for the next submitted (non-blocking) steps. */
		refuseStep: [],
		/** Resolvers of requests currently sitting in the blocking step. */
		waiters: [],
		/** The logins this bridge pretends to hold. */
		logins: [],
		starts: [],
		submits: [],
		cancelled: [],
		loggedOut: [],
		refuseStart: [],
		blockingArrivals: 0,
		held: 0,
		nextProcess: 0,
		nextQr: 0
	};
}

function mautrixError(response, status, errcode) {
	json(response, status, { errcode, error: `the stub bridge answers ${errcode}` });
}

function json(response, status, body) {
	const payload = JSON.stringify(body);
	response.writeHead(status, {
		'content-type': 'application/json',
		'content-length': Buffer.byteLength(payload)
	});
	response.end(payload);
}

function qrStep(processId, data) {
	return {
		login_process_id: processId,
		type: 'display_and_wait',
		step_id: QR_STEP,
		instructions: 'Scan this code from the phone',
		// The raw payload. A real bridge renders no image: the browser draws it.
		display_and_wait: { type: 'qr', data, can_cancel: true }
	};
}

/**
 * A step a test queued, as the document a bridge answers a submit with.
 *
 * The caller says what the step asks for and the stub fills in what a live
 * bridge invents per call — the process id — so a spec never hand-writes the
 * envelope. The payload member is named after the step type, which is
 * bridgev2's own shape: `user_input: {fields}`, `cookies: {url, fields}`,
 * `display_and_wait: {type, data}`.
 */
function queuedStep(processId, spec) {
	const type = spec.type ?? 'user_input';
	const step = {
		login_process_id: processId,
		type,
		step_id: spec.step_id ?? `fi.mau.stub.login.${type}`,
		instructions: spec.instructions ?? ''
	};
	switch (type) {
		case 'display_and_wait':
			step.display_and_wait = { type: spec.payload_type ?? 'qr', data: spec.data ?? '' };
			break;
		case 'cookies':
			step.cookies = { url: spec.url ?? null, fields: spec.fields ?? [] };
			break;
		default:
			// `attachments: null` explicitly, as mautrix-whatsapp's captured
			// `user_input` answer carries it.
			step.user_input = { fields: spec.fields ?? [], attachments: null };
	}
	return step;
}

/**
 * A login the bridge holds, shaped as `whoami` describes one.
 *
 * `state` is the nested `BridgeState` the reference bridges put there, and it
 * is what the Companion's connected badge is read from since #108. A login
 * with no `state` is a bridge that has just restarted — its state lives in
 * memory — and reads as `starting`, not as disconnected.
 */
function heldLogin(loginId, name, stateEvent = 'CONNECTED') {
	const login = { id: loginId, name, profile: { phone: name } };
	if (stateEvent !== null) {
		login.state = {
			state_event: stateEvent,
			timestamp: 1789706879,
			ttl: 21600,
			source: 'bridge'
		};
	}
	return login;
}

function completion(bridge, processId, loginId) {
	bridge.processes.delete(processId);
	if (!bridge.logins.some((login) => login.id === loginId)) {
		// A real bridge connects the session it has just been given, and then
		// reports `CONNECTED`. The stub does both at once, so a journey that
		// scans a code can assert the card that follows.
		bridge.logins.push(heldLogin(loginId, 'the stub’s account'));
	}
	return {
		type: 'complete',
		step_id: 'fi.mau.stub.login.complete',
		instructions: 'Connected',
		complete: { login_id: loginId, user_login_id: loginId }
	};
}

async function readBody(request) {
	const chunks = [];
	for await (const chunk of request) {
		chunks.push(chunk);
	}
	const text = Buffer.concat(chunks).toString('utf8').trim();
	if (text === '') {
		return null;
	}
	try {
		return JSON.parse(text);
	} catch {
		return text;
	}
}

/**
 * Starts the stub on a free loopback port.
 *
 * `bridgeIds` are the instances it serves. `hooks.signIn` is what the control
 * surface's sign-in calls out to — the stub knows nothing about Matrix, and the
 * orchestrator that does passes it in.
 */
export async function startStubBridge(bridgeIds, hooks = {}) {
	const bridges = new Map(bridgeIds.map((id) => [id, freshBridge()]));

	/**
	 * Hands queued releases to the requests sitting in a blocking step, one
	 * each. A waiter that could not take one goes back to the head of the
	 * queue: a release is consumed by exactly one held request, and a release
	 * queued before any request arrives simply waits for it.
	 */
	function pump(bridge) {
		while (bridge.waiters.length > 0 && bridge.releases.length > 0) {
			const wake = bridge.waiters.shift();
			if (!wake()) {
				bridge.waiters.unshift(wake);
				return;
			}
		}
	}

	function queue(bridge, release) {
		bridge.releases.push(release);
		pump(bridge);
	}

	async function heldStep(bridge, processId, response) {
		bridge.blockingArrivals += 1;
		bridge.held += 1;
		const release = await new Promise((resolve) => {
			const take = () => {
				const next = bridge.releases.shift();
				if (next !== undefined) {
					resolve(next);
					return true;
				}
				return false;
			};
			if (!take()) {
				bridge.waiters.push(take);
			}
		});
		bridge.held -= 1;
		switch (release.kind) {
			case 'qr':
				json(response, 200, qrStep(processId, release.data));
				return;
			case 'step':
				// A blocking step answered with a **question**: the shape a
				// Telegram QR login takes when the account has two-factor
				// authentication on, which is the case #175 found dead-ending on
				// a blank screen.
				json(response, 200, queuedStep(processId, release.step));
				return;
			case 'complete':
				json(response, 200, completion(bridge, processId, release.loginId));
				return;
			default:
				mautrixError(response, release.status, release.errcode);
		}
	}

	const server = createServer((request, response) => {
		void handle(request, response).catch((error) => {
			response.writeHead(500, { 'content-type': 'text/plain' });
			response.end(`${error}\n`);
		});
	});

	async function handle(request, response) {
		const url = new URL(request.url ?? '/', 'http://stub.invalid');
		const segments = url.pathname.split('/').filter((part) => part !== '');

		if (segments[0] === 'stub-control') {
			await control(request, response, segments.slice(1), url);
			return;
		}
		if (segments[0] !== 'bridges' || segments[1] === undefined) {
			mautrixError(response, 404, 'M_NOT_FOUND');
			return;
		}
		const bridge = bridges.get(segments[1]);
		if (bridge === undefined) {
			mautrixError(response, 404, 'M_NOT_FOUND');
			return;
		}
		// Every provisioning endpoint checks the bearer secret first, exactly as
		// a real bridge does — and answers `M_FORBIDDEN` without it.
		const authorization = request.headers['authorization'];
		if (authorization !== `Bearer ${STUB_PROVISIONING_SECRET}`) {
			mautrixError(response, 403, 'M_FORBIDDEN');
			return;
		}
		await provision(request, response, bridge, segments.slice(2), url);
	}

	async function provision(request, response, bridge, rest, url) {
		// `_matrix/provision/v3/…`
		const path = rest.slice(3);
		const head = path.join('/');

		if (head === 'login/flows' && request.method === 'GET') {
			json(response, 200, {
				flows: [
					{ id: QR_FLOW, name: 'Scan a QR code', description: 'Link a device by scanning' },
					{ id: PHONE_FLOW, name: 'Phone number' },
					{ id: COOKIES_FLOW, name: 'Paste cookies' }
				]
			});
			return;
		}

		if (head === 'logins' && request.method === 'GET') {
			// Bare id strings under `login_ids` — what mautrix-whatsapp and
			// mautrix-signal v26.09 really answer (#106). No name, no profile,
			// no state: those live in whoami and nowhere else.
			json(response, 200, { login_ids: bridge.logins.map((login) => login.id) });
			return;
		}

		if (head === 'whoami' && request.method === 'GET') {
			// The only call that describes a login, and therefore the only
			// honest source of "is this network connected?" (#108). Each entry
			// carries the same nested `BridgeState` document the status
			// webhook pushes.
			json(response, 200, {
				network: { id: 'stub', display_name: 'Stub' },
				logins: bridge.logins
			});
			return;
		}

		if (path[0] === 'login' && path[1] === 'start' && request.method === 'POST') {
			const flowId = path[2] ?? '';
			bridge.starts.push({
				flow_id: flowId,
				user_id: url.searchParams.get('user_id'),
				login_id: url.searchParams.get('login_id')
			});
			const refusal = bridge.refuseStart.shift();
			if (refusal !== undefined) {
				mautrixError(response, refusal.status, refusal.errcode);
				return;
			}
			bridge.nextProcess += 1;
			bridge.nextQr += 1;
			const processId = `stub-process-${bridge.nextProcess}`;
			bridge.processes.set(processId, flowId);
			switch (flowId) {
				case QR_FLOW:
					json(response, 200, qrStep(processId, `2@stub-qr-payload-${bridge.nextQr}`));
					return;
				case PHONE_FLOW:
					json(response, 200, {
						login_process_id: processId,
						type: 'user_input',
						step_id: PHONE_STEP,
						instructions: 'Enter the phone number of the account',
						user_input: {
							fields: [{ type: 'phone_number', id: 'phone_number', name: 'Phone number' }]
						}
					});
					return;
				case COOKIES_FLOW:
					json(response, 200, {
						login_process_id: processId,
						type: 'cookies',
						step_id: COOKIES_STEP,
						instructions: 'Paste the cookies from a private window',
						cookies: {
							url: 'https://messages.google.com/web/authentication',
							// bridgev2's own `LoginCookieField`: a field has no type of
							// its own — it carries `sources`, each with the type, the
							// name the value goes by in the browser, and the domain
							// (`mautrix/go`, `bridgev2/login.go`). The stub used to
							// invent a `type` on the field itself, which is the shape
							// no bridge sends.
							fields: [
								{
									id: 'SID',
									required: true,
									sources: [{ type: 'cookie', name: 'SID', cookie_domain: '.google.com' }]
								},
								{
									id: 'SAPISID',
									required: true,
									sources: [{ type: 'cookie', name: 'SAPISID', cookie_domain: '.google.com' }]
								}
							]
						}
					});
					return;
				default:
					mautrixError(response, 404, 'M_NOT_FOUND');
			}
			return;
		}

		if (path[0] === 'login' && path[1] === 'step' && request.method === 'POST') {
			const processId = path[2] ?? '';
			const stepId = path[3] ?? '';
			const stepType = path[4] ?? '';
			if (!bridge.processes.has(processId)) {
				// What a restarted bridge answers, and the one case the Gateway
				// must report as a lost login.
				mautrixError(response, 404, 'M_NOT_FOUND');
				return;
			}
			if (stepType === 'cancel') {
				queue(bridge, { kind: 'refusal', status: 409, errcode: 'FI.MAU.LOGIN_STEP_CANCELLED' });
				response.writeHead(204);
				response.end();
				return;
			}
			const body = await readBody(request);
			// bridgev2 decodes a submit's body into `map[string]string` before
			// any connector runs, so a value that is not a string — a cookie jar
			// sent as an object, which the Companion did until #224 — is refused
			// by the decoder as `M_NOT_JSON`, and the login process survives it:
			// only a connector's refusal reaches `deleteLogin` (#249). The stub
			// says the same, so a journey that reads the jar off `stats()` reads
			// the wire shape and not a convenience of the stub's (#267).
			if (body !== null && typeof body === 'object') {
				const wrong = Object.entries(body).find(([, value]) => typeof value !== 'string');
				if (wrong !== undefined) {
					mautrixError(response, 400, 'M_NOT_JSON');
					return;
				}
			}
			bridge.submits.push({
				step_id: stepId,
				step_type: stepType,
				body,
				txn_id: url.searchParams.get('txn_id')
			});
			if (stepType === 'display_and_wait') {
				await heldStep(bridge, processId, response);
				return;
			}
			const refused = bridge.refuseStep.shift();
			if (refused !== undefined) {
				// A `400` destroys the login process on a real bridge — the
				// capture is explicit that every later call against it, the
				// cancels included, answered `404` — so there is no retrying a
				// refused step (`fixtures/mautrix-whatsapp/login-step.json`,
				// answer `rejected_user_input`).
				if (refused.status === 400) {
					bridge.processes.delete(processId);
				}
				mautrixError(response, refused.status, refused.errcode);
				return;
			}
			const queued = bridge.nextSteps.shift();
			if (queued !== undefined) {
				json(response, 200, queuedStep(processId, queued));
				return;
			}
			// The SMS preview path: cookies are answered with the emoji pairing
			// step, which is the same blocking step with a different payload
			// type, and mautrix-gmessages really does answer that way.
			if (stepType === 'cookies') {
				json(response, 200, {
					login_process_id: processId,
					type: 'display_and_wait',
					step_id: EMOJI_STEP,
					instructions: 'Check that this emoji matches the one on your phone',
					display_and_wait: { type: 'emoji', data: '🐢', can_cancel: true }
				});
				return;
			}
			json(response, 200, completion(bridge, processId, `stub-login-${processId}`));
			return;
		}

		if (path[0] === 'login' && path[1] === 'cancel' && request.method === 'POST') {
			const processId = path[2] ?? '';
			if (!bridge.processes.delete(processId)) {
				mautrixError(response, 404, 'M_NOT_FOUND');
				return;
			}
			bridge.cancelled.push(processId);
			response.writeHead(204);
			response.end();
			return;
		}

		if (path[0] === 'logout' && request.method === 'POST') {
			const loginId = path[1] ?? '';
			const before = bridge.logins.length;
			bridge.logins = bridge.logins.filter((login) => login.id !== loginId);
			if (bridge.logins.length === before) {
				mautrixError(response, 404, 'M_NOT_FOUND');
				return;
			}
			bridge.loggedOut.push(loginId);
			response.writeHead(204);
			response.end();
			return;
		}

		mautrixError(response, 404, 'M_NOT_FOUND');
	}

	async function control(request, response, rest, url) {
		// `/stub-control/sign-in` is the orchestrator's, not a bridge's.
		if (rest[0] === 'sign-in') {
			if (hooks.signIn === undefined) {
				json(response, 501, { error: 'no sign-in hook' });
				return;
			}
			const answer = await hooks.signIn(url.searchParams.get('device') ?? 'the test’s device');
			json(response, 200, answer);
			return;
		}
		if (rest[0] === 'matrix-user') {
			if (hooks.matrixUser === undefined) {
				json(response, 501, { error: 'no matrix-user hook' });
				return;
			}
			json(response, 200, await hooks.matrixUser(url.searchParams.get('localpart')));
			return;
		}

		const bridge = bridges.get(rest[0] ?? '');
		if (bridge === undefined) {
			json(response, 404, { error: 'unknown bridge', bridge: rest[0] });
			return;
		}
		const action = rest[1] ?? '';
		const body = (await readBody(request)) ?? {};

		switch (action) {
			case 'release-qr':
				// A refreshed code: the same step, a new payload. What a real
				// bridge does every ~20 seconds while nobody has scanned.
				bridge.nextQr += 1;
				queue(bridge, {
					kind: 'qr',
					data: body.data ?? `2@stub-qr-payload-refreshed-${bridge.nextQr}`
				});
				json(response, 200, { released: 'qr' });
				return;
			case 'release-step':
				// The held blocking step answers with another step instead of a
				// code or a completion — a QR flow that interjects a question.
				queue(bridge, { kind: 'step', step: body });
				json(response, 200, { released: 'step', step_id: body.step_id ?? null });
				return;
			case 'release-complete':
				queue(bridge, { kind: 'complete', loginId: body.login_id ?? 'stub-login-complete' });
				json(response, 200, { released: 'complete' });
				return;
			case 'release-refusal':
				queue(bridge, {
					kind: 'refusal',
					status: body.status ?? 502,
					errcode: body.errcode ?? 'M_UNKNOWN'
				});
				json(response, 200, { released: 'refusal' });
				return;
			case 'queue-next-step':
				// The control the renderer's central case needs: the next
				// submit is answered with another step instead of the
				// completion, so a sequence — a phone number then a code then
				// a two-factor password — can be driven from a browser test.
				bridge.nextSteps.push(body);
				json(response, 200, { queued: 'step', step_id: body.step_id ?? null });
				return;
			case 'refuse-next-submit':
				// What a network refusing a value looks like: the bridge
				// answers `400` with the connector's own errcode and drops the
				// login process with it.
				bridge.refuseStep.push({
					status: body.status ?? 400,
					errcode: body.errcode ?? 'FI.MAU.STUB.VALUE_REFUSED'
				});
				json(response, 200, { queued: 'refusal' });
				return;
			case 'refuse-next-start':
				bridge.refuseStart.push({
					status: body.status ?? 502,
					errcode: body.errcode ?? 'M_UNKNOWN'
				});
				json(response, 200, { queued: 'refusal' });
				return;
			case 'restart': {
				// A bridge restart, as the Gateway can tell one: every process
				// forgotten, and every held request answered with the `404` a
				// bridge gives for a process it has never heard of.
				const held = bridge.held;
				bridge.processes.clear();
				bridge.releases.length = 0;
				bridge.nextSteps.length = 0;
				bridge.refuseStep.length = 0;
				for (let index = 0; index < Math.max(held, 1); index += 1) {
					queue(bridge, { kind: 'refusal', status: 404, errcode: 'M_NOT_FOUND' });
				}
				json(response, 200, { restarted: true });
				return;
			}
			case 'add-login':
				bridge.logins.push(
					heldLogin(
						body.login_id ?? 'existing-login',
						body.name ?? 'an account the bridge already holds',
						body.state_event === undefined ? 'CONNECTED' : body.state_event
					)
				);
				json(response, 200, { added: true });
				return;
			case 'set-login-state': {
				// The state a bridge reports for one of its logins, in
				// mautrix's own vocabulary. `null` is a bridge that holds the
				// login and has said nothing about it yet.
				const target = bridge.logins.find((login) => login.id === body.login_id);
				if (target === undefined) {
					json(response, 404, { error: 'no such login', login_id: body.login_id });
					return;
				}
				if (body.state_event === null) {
					delete target.state;
				} else {
					target.state = {
						state_event: body.state_event,
						timestamp: body.timestamp ?? 1789706879,
						ttl: 21600,
						source: 'bridge'
					};
				}
				json(response, 200, { set: true });
				return;
			}
			case 'stats':
				json(response, 200, {
					blocking_arrivals: bridge.blockingArrivals,
					held: bridge.held,
					starts: bridge.starts,
					submits: bridge.submits,
					cancelled: bridge.cancelled,
					logged_out: bridge.loggedOut,
					logins: bridge.logins
				});
				return;
			case 'reset':
				bridges.set(rest[0], freshBridge());
				json(response, 200, { reset: true });
				return;
			default:
				json(response, 404, { error: 'unknown control action', action });
		}
	}

	await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
	const { port } = server.address();
	return {
		port,
		origin: `http://127.0.0.1:${port}`,
		/** The URL the Gateway is configured with for one instance. */
		bridgeUrl: (bridgeId) => `http://127.0.0.1:${port}/bridges/${bridgeId}`,
		close: () =>
			new Promise((resolve) => {
				server.closeAllConnections?.();
				server.close(resolve);
			})
	};
}
