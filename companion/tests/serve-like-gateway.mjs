// The Companion's built export, served the way the Companion Gateway serves
// it. Playwright runs against this.
//
// This is not a convenience static server. Spec #65 is blunt about why: "the
// test server must implement the same fallback and `.html` resolution as the
// Gateway, or the tests prove a routing contract nobody ships". So every rule
// below is transcribed from `companion-gateway/src/static_files.rs` and
// `src/http.rs`, in their order, and the module docs there are the source:
//
//   1. the exact file at the path;
//   2. else the path plus `index.html` when it ends in `/`, else plus `.html`;
//   3. else, a trailing-slash path whose slash-less `.html` exists: 307;
//   4. else, a slash-less path whose `<path>/index.html` exists: 307;
//   5. on a match, 200 with the content type of the path's extension
//      (`text/html` when it has none);
//   6. else the fallback file (`200.html`) with **200** — never a redirect,
//      never a 404.
//
// Plus the parts of the origin that are not files:
//
//   - `/health`, whose `version` is the server half of the version handshake.
//     `TWALK_TEST_GATEWAY_VERSION` moves it, which is how the mismatch is
//     tested without a stale build.
//   - `/api` and `/api/*`: a JSON refusal, never the app shell. Under the
//     Gateway's guard a browser with no device cookie gets `401
//     unauthenticated`, and that is what a signed-out Companion sees.
//   - `/openapi.yaml`: the same bytes the Gateway embeds.
//   - a pre-compressed sibling (`.br`, `.gz`) served with the matching
//     `Content-Encoding` to a client that accepts it, the content type still
//     taken from the *original* extension — which is what keeps `.wasm`
//     exactly `application/wasm` whichever encoding went out.
//
// `.wasm` as exactly `application/wasm`, with no parameters, is the one the
// tests assert hardest: `WebAssembly.instantiateStreaming` refuses anything
// else with a `TypeError` and has no fallback, and the Matrix crypto stack of
// ADR 0014 is loaded that way.

import { createReadStream } from 'node:fs';
import { stat, readFile } from 'node:fs/promises';
import { createServer, request as httpRequest } from 'node:http';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

const here = fileURLToPath(new URL('.', import.meta.url));

const ROOT = resolve(process.env.TWALK_TEST_STATIC_DIR ?? join(here, '..', 'build'));
const FALLBACK = process.env.TWALK_TEST_FALLBACK_FILE ?? '200.html';
const PORT = Number(process.env.TWALK_TEST_PORT ?? 4319);
const GATEWAY_VERSION = process.env.TWALK_TEST_GATEWAY_VERSION ?? '0.1.0';
const GATEWAY_REVISION = process.env.TWALK_TEST_GATEWAY_REVISION ?? 'test';
const OPENAPI = resolve(join(here, '..', '..', 'companion-gateway', 'openapi.yaml'));
/**
 * With `TWALK_TEST_REAL_STACK=1` this server stops standing in for the API and
 * puts the **real Companion Gateway** behind `/api` — with a real Synapse
 * behind that (`tests/real-stack.mjs`). Spec #65 asks for exactly this for the
 * bootstrap journey: the cryptographic work of ADR 0014 is round trips to a
 * homeserver, and a stub answering them would assert nothing.
 *
 * The files stay ours, served by the rules below, so the routing contract this
 * file exists to prove is still the one under test. Only `/api` moves, and it
 * moves to the same origin the browser already has — which is what keeps the
 * device cookie (`HttpOnly`, ADR 0011) working exactly as it does in a
 * deployment.
 */
const REAL_STACK = process.env.TWALK_TEST_REAL_STACK === '1';
/**
 * `TWALK_TEST_BRIDGE_STACK=1` asks for the same thing with the bridge journeys'
 * Gateway instead (ticket #68): an owner whose account already exists, and a
 * stub bridge whose control surface this origin also exposes, under
 * `/stub-control`. Two origins because there are two owners — see
 * `playwright.config.ts`.
 */
const BRIDGE_STACK = process.env.TWALK_TEST_BRIDGE_STACK === '1';
/**
 * `TWALK_TEST_SESSION_STACK=1` asks for the session journeys' Gateway instead
 * (ticket #111): the same owner and the same stub bridges as the bridge stack,
 * on a deployment whose device token lives five seconds, so a browser test can
 * watch a credential expire under a working screen.
 */
const SESSION_STACK = process.env.TWALK_TEST_SESSION_STACK === '1';
let gatewayOrigin = process.env.TWALK_TEST_GATEWAY_PROXY ?? null;
let stubOrigin = process.env.TWALK_TEST_STUB_PROXY ?? null;

/** `companion-gateway/src/static_files.rs::content_type_for`, value for value. */
function contentTypeFor(requestPath) {
	const name = requestPath.split('/').pop() ?? '';
	const dot = name.lastIndexOf('.');
	const extension = dot === -1 ? null : name.slice(dot + 1).toLowerCase();
	switch (extension) {
		case 'wasm':
			return 'application/wasm';
		case 'js':
		case 'mjs':
			return 'text/javascript; charset=utf-8';
		case 'css':
			return 'text/css; charset=utf-8';
		case 'json':
		case 'map':
			return 'application/json';
		case 'webmanifest':
			return 'application/manifest+json';
		case 'svg':
			return 'image/svg+xml';
		case 'png':
			return 'image/png';
		case 'jpg':
		case 'jpeg':
			return 'image/jpeg';
		case 'gif':
			return 'image/gif';
		case 'webp':
			return 'image/webp';
		case 'avif':
			return 'image/avif';
		case 'ico':
			return 'image/vnd.microsoft.icon';
		case 'woff2':
			return 'font/woff2';
		case 'woff':
			return 'font/woff';
		case 'ttf':
			return 'font/ttf';
		case 'txt':
			return 'text/plain; charset=utf-8';
		case 'xml':
			return 'application/xml';
		case 'pdf':
			return 'application/pdf';
		case 'mp4':
			return 'video/mp4';
		case 'webm':
			return 'video/webm';
		default:
			// `html`, no extension, and anything unknown.
			return 'text/html; charset=utf-8';
	}
}

/** `safe_relative_path`: a path that climbs out of the build is not a path. */
function safeRelativePath(requestPath) {
	const out = [];
	for (const segment of requestPath.split('/')) {
		if (segment === '' || segment === '.') {
			continue;
		}
		let decoded;
		try {
			decoded = decodeURIComponent(segment);
		} catch {
			return null;
		}
		if (
			decoded === '' ||
			decoded === '.' ||
			decoded === '..' ||
			decoded.includes('/') ||
			decoded.includes('\\') ||
			decoded.includes('\0')
		) {
			return null;
		}
		out.push(decoded);
	}
	return out;
}

async function isFile(path) {
	try {
		return (await stat(path)).isFile();
	} catch {
		return false;
	}
}

/** The Gateway's resolution order. Returns what to do, not how to do it. */
async function resolvePath(requestPath) {
	const trailingSlash = requestPath.endsWith('/');
	const segments = safeRelativePath(requestPath);
	if (segments === null) {
		return fallback();
	}
	const relative = segments.join('/');

	// 1. The exact file.
	if (relative !== '' && !trailingSlash) {
		const candidate = join(ROOT, relative);
		if (await isFile(candidate)) {
			return { kind: 'file', path: candidate, contentType: contentTypeFor(requestPath) };
		}
	}

	// 2. The prerendered page under the spelling this path asks for.
	const derived =
		trailingSlash || relative === ''
			? join(ROOT, relative, 'index.html')
			: join(ROOT, `${relative}.html`);
	if (await isFile(derived)) {
		return { kind: 'file', path: derived, contentType: contentTypeFor(requestPath) };
	}

	// 3 and 4. The build has the page under the other spelling.
	if (trailingSlash && relative !== '' && (await isFile(join(ROOT, `${relative}.html`)))) {
		return { kind: 'redirect', location: requestPath.replace(/\/+$/, '') };
	}
	if (!trailingSlash && relative !== '' && (await isFile(join(ROOT, relative, 'index.html')))) {
		return { kind: 'redirect', location: `${requestPath}/` };
	}

	// 6. A client-side route: the app shell answers it, with 200.
	return fallback();
}

async function fallback() {
	const path = join(ROOT, FALLBACK);
	return (await isFile(path))
		? { kind: 'fallback', path, contentType: 'text/html; charset=utf-8' }
		: { kind: 'not-found' };
}

/**
 * Serves one file, preferring a pre-compressed sibling the client accepts —
 * `tower_http`'s `ServeFile::precompressed_br`, which the Gateway turns on.
 * The content type stays the original extension's.
 */
async function serveFile(request, response, path, contentType) {
	const accepted = String(request.headers['accept-encoding'] ?? '');
	for (const [encoding, suffix] of [
		['br', '.br'],
		['gzip', '.gz']
	]) {
		if (accepted.includes(encoding) && (await isFile(`${path}${suffix}`))) {
			const info = await stat(`${path}${suffix}`);
			response.writeHead(200, {
				'content-type': contentType,
				'content-encoding': encoding,
				'content-length': info.size,
				vary: 'accept-encoding'
			});
			createReadStream(`${path}${suffix}`).pipe(response);
			return;
		}
	}
	const info = await stat(path);
	response.writeHead(200, {
		'content-type': contentType,
		'content-length': info.size,
		vary: 'accept-encoding'
	});
	createReadStream(path).pipe(response);
}

/**
 * Hands one request to a real backend and streams its answer back, headers
 * included — `Set-Cookie` above all, since the session is a cookie.
 *
 * Two backends use it: the real Companion Gateway under `/api`, and the stub
 * bridge's control surface under `/stub-control` (ticket #68), which is how a
 * browser journey drives both sides of a bridge login from one origin.
 *
 * The `Host` header is forwarded unchanged on purpose: the Gateway decides
 * whether its session cookies carry `Secure` from it
 * (`companion-gateway/src/session_http.rs::secure_origin`), and on loopback
 * they must not, or the browser would drop them over plain HTTP.
 */
function proxyToGateway(request, response, origin) {
	const target = new URL(request.url ?? '/', origin);
	const upstream = httpRequest(
		{
			protocol: target.protocol,
			hostname: target.hostname,
			port: target.port,
			path: `${target.pathname}${target.search}`,
			method: request.method,
			headers: request.headers
		},
		(answer) => {
			response.writeHead(answer.statusCode ?? 502, answer.headers);
			answer.pipe(response);
		}
	);
	upstream.on('error', (error) => {
		json(response, 502, { error: 'gateway_unreachable', detail: String(error) });
	});
	request.pipe(upstream);
}

function json(response, status, body) {
	const payload = JSON.stringify(body);
	response.writeHead(status, {
		'content-type': 'application/json',
		'content-length': Buffer.byteLength(payload)
	});
	response.end(payload);
}

const server = createServer((request, response) => {
	void (async () => {
		const url = new URL(request.url ?? '/', `http://localhost:${PORT}`);
		const path = url.pathname;

		if (path === '/health') {
			json(response, 200, {
				status: 'ok',
				version: GATEWAY_VERSION,
				revision: GATEWAY_REVISION
			});
			return;
		}

		if (path === '/metrics') {
			response.writeHead(200, { 'content-type': 'text/plain; version=0.0.4; charset=utf-8' });
			response.end('# the Gateway exposes its own metrics here\n');
			return;
		}

		if (path === '/openapi.yaml') {
			const body = await readFile(OPENAPI).catch(() => null);
			if (body === null) {
				json(response, 404, { error: 'not_found', path });
				return;
			}
			response.writeHead(200, {
				'content-type': 'application/yaml',
				'content-length': body.length
			});
			response.end(body);
			return;
		}

		// The stub bridge's control surface, when one is running: a test drives
		// both sides of a login — the browser and the bridge — from this one
		// origin. Never present unless the bridge stack started one.
		if (stubOrigin !== null && path.startsWith('/stub-control/')) {
			proxyToGateway(request, response, stubOrigin);
			return;
		}

		// The one exception to the fallback: under `/api` a refusal stays
		// JSON, because a client parsing an API response must never be handed
		// an HTML page. A browser with no device cookie gets 401 — unless a
		// real Gateway is behind us, in which case it answers for itself.
		if (path === '/api' || path.startsWith('/api/')) {
			if (gatewayOrigin !== null) {
				proxyToGateway(request, response, gatewayOrigin);
				return;
			}
			json(response, 401, { error: 'unauthenticated' });
			return;
		}

		const resolved = await resolvePath(path);
		switch (resolved.kind) {
			case 'file':
			case 'fallback':
				await serveFile(request, response, resolved.path, resolved.contentType);
				return;
			case 'redirect':
				response.writeHead(307, {
					location: `${resolved.location}${url.search}`
				});
				response.end();
				return;
			default:
				response.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
				response.end(`The Companion is not available: no build in ${ROOT}.\n`);
		}
	})().catch((error) => {
		response.writeHead(500, { 'content-type': 'text/plain; charset=utf-8' });
		response.end(`${error}\n`);
	});
});

// The real stack, when asked for, comes up *before* the origin answers
// anything: Playwright waits on `/health`, so a server that is listening is a
// server whose Gateway and Synapse are ready.
if ((REAL_STACK || BRIDGE_STACK || SESSION_STACK) && gatewayOrigin === null) {
	const { startBridgeStack, startRealStack, startSessionStack } = await import('./real-stack.mjs');
	const stack = SESSION_STACK
		? await startSessionStack()
		: BRIDGE_STACK
			? await startBridgeStack()
			: await startRealStack();
	gatewayOrigin = stack.gatewayOrigin;
	stubOrigin = stack.stubOrigin ?? null;
	console.log(`the real Gateway answers /api at ${gatewayOrigin}`);
	console.log(`the real Synapse is ${stack.synapseUrl}, owner ${stack.ownerId}`);
	if (stubOrigin !== null) {
		console.log(`the stub bridge answers /stub-control at ${stubOrigin}`);
	}
}

server.listen(PORT, '127.0.0.1', () => {
	console.log(`serving ${ROOT} like the Gateway on http://127.0.0.1:${PORT}`);
	console.log(`/health reports version ${GATEWAY_VERSION}`);
	if (gatewayOrigin !== null) {
		console.log(`/api is proxied to ${gatewayOrigin}`);
	}
});
