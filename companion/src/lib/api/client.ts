// The Gateway client. Not a client implementation: `openapi-fetch` is a
// generic runtime, and every path, parameter and response shape it checks
// comes from `schema.d.ts`, which `scripts/generate-api-client.mjs` generates
// from `companion-gateway/openapi.yaml`. Nothing about the Gateway's HTTP
// surface is written down here — a route, a request body or a response member
// that this file appeared to know about would be exactly the drift the ticket
// forbids.
//
// Two configuration facts, and they are the whole file:
//
//   - `baseUrl: ''`. The Gateway serves the Companion's own files, so its API
//     is same-origin and every request is relative. That is also what makes
//     the device token an `HttpOnly` cookie (ADR 0011): there is no CORS to
//     negotiate and no `Authorization` header to build.
//
//   - `credentials: 'same-origin'`. The browser attaches `twalk_device` (and,
//     on `/api/session`, `twalk_refresh`) itself. `HttpOnly` hides both from
//     JavaScript, so this code must never try to read them.

import createClient from 'openapi-fetch';

import type { paths } from './schema';

// One rule for the screens that will use this client, written here because
// there is nowhere better: branch on a refusal's `error` code, which the
// description guarantees is stable, and never on its `detail`, which is an
// operator's sentence and is not for display.
export const gateway = createClient<paths>({
	baseUrl: '',
	credentials: 'same-origin'
});
