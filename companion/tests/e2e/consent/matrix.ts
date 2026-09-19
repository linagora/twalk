// A conversation, over the Matrix client-server API and nothing else.
//
// The consent journey needs a real room with a real second party in it, so that
// a real Sensor has something to observe and to label. That is three HTTP calls
// and a poll, which is why this is forty lines of `fetch` rather than a
// dependency: `matrix-js-sdk` is in the app's manifest for the cryptographic
// bootstrap (ADR 0014), and bringing a client up here would mean an olm stack, a
// device and a store, for a room whose whole purpose is to be unencrypted.
//
// **Unencrypted, deliberately.** The Sensor can read an encrypted room (it runs
// matrix-sdk with its crypto stack, and `sensor/tests/harness/crypto.rs` exists
// for exactly that), but a plain-HTTP sender cannot Megolm-encrypt, and what
// this journey is about is the `consent` label on the envelope rather than the
// message's transport. The same reasoning as the Sensor harness's own HTTP
// `Bot`.
//
// The password scheme is `tests/harness/scripts/provision-bots.sh`'s, stated
// there as a contract: *"the Sensor harness's Bot helper logs in with the same
// scheme — keep in sync."* Throwaway constants for a local stack.

export interface MatrixUser {
	userId: string;
	accessToken: string;
}

/** Logs a provisioned test account in. */
export async function logIn(synapseUrl: string, localpart: string): Promise<MatrixUser> {
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
	const body = (await answer.json()) as { user_id: string; access_token: string };
	return { userId: body.user_id, accessToken: body.access_token };
}

/**
 * Creates a room with no encryption and invites the users named.
 *
 * `is_direct` and no `m.room.encryption`: what the Sensor's own harness creates
 * for an observable conversation.
 */
export async function createRoom(
	synapseUrl: string,
	user: MatrixUser,
	name: string,
	invite: readonly string[]
): Promise<string> {
	const answer = await fetch(`${synapseUrl}/_matrix/client/v3/createRoom`, {
		method: 'POST',
		headers: {
			'content-type': 'application/json',
			authorization: `Bearer ${user.accessToken}`
		},
		body: JSON.stringify({ name, is_direct: true, invite: [...invite] })
	});
	if (!answer.ok) {
		throw new Error(
			`the homeserver refused to create a room: ${answer.status} ${await answer.text()}`
		);
	}
	return ((await answer.json()) as { room_id: string }).room_id;
}

/** Joins a room this user was invited to. */
export async function joinRoom(
	synapseUrl: string,
	user: MatrixUser,
	roomId: string
): Promise<void> {
	const answer = await fetch(
		`${synapseUrl}/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}/join`,
		{ method: 'POST', headers: { authorization: `Bearer ${user.accessToken}` }, body: '{}' }
	);
	if (!answer.ok) {
		throw new Error(`${user.userId} could not join ${roomId}: ${answer.status}`);
	}
}

/** Sends one plain-text message, and returns the event id the homeserver gave it. */
export async function sendMessage(
	synapseUrl: string,
	user: MatrixUser,
	roomId: string,
	body: string
): Promise<string> {
	const transaction = `twalk-consent-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
	const answer = await fetch(
		`${synapseUrl}/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}` +
			`/send/m.room.message/${transaction}`,
		{
			method: 'PUT',
			headers: {
				'content-type': 'application/json',
				authorization: `Bearer ${user.accessToken}`
			},
			body: JSON.stringify({ msgtype: 'm.text', body })
		}
	);
	if (!answer.ok) {
		throw new Error(`${user.userId} could not send into ${roomId}: ${answer.status}`);
	}
	return ((await answer.json()) as { event_id: string }).event_id;
}

/**
 * Waits until a user's membership in a room is what is expected.
 *
 * Asked of the **homeserver**, not of a log line: "the Sensor joined" is a fact
 * about room state, and `CONTRIBUTING.md` asks for exactly that — *"ask the
 * homeserver whether a room was joined"*.
 */
export async function waitForMembership(
	synapseUrl: string,
	user: MatrixUser,
	roomId: string,
	who: string,
	membership: string,
	timeoutMs = 60_000
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	let last = 'nothing yet';
	for (;;) {
		const answer = await fetch(
			`${synapseUrl}/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}` +
				`/state/m.room.member/${encodeURIComponent(who)}`,
			{ headers: { authorization: `Bearer ${user.accessToken}` } }
		);
		if (answer.ok) {
			const body = (await answer.json()) as { membership?: string };
			if (body.membership === membership) {
				return;
			}
			last = body.membership ?? 'none';
		} else {
			last = `HTTP ${answer.status}`;
		}
		if (Date.now() > deadline) {
			throw new Error(
				`${who} is "${last}" in ${roomId} after ${timeoutMs} ms, not "${membership}"`
			);
		}
		await new Promise((resolve) => setTimeout(resolve, 500));
	}
}
