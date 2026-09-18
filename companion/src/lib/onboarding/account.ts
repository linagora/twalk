// What screen 2's first step accepts before it will call the Gateway.
//
// The rules are the wireframe's, not Matrix's and not Synapse's: "3 to 30
// characters, lowercase, digits, underscore, hyphen" for the username, and
// "minimum 12 characters, at least one letter and one digit" with a strength
// meter for the password. Validating here does not remove the homeserver's own
// policy — it is authoritative and its refusal is mapped back onto these same
// fields (`M_PASSWORD_TOO_SHORT`, `M_USER_IN_USE`) — it removes the round trip
// for the mistakes we can already see.
//
// Pure, so the rules are unit-tested rather than clicked through.

/** Why a username will not do. One message per value, in the catalogues. */
export type UsernameProblem = "empty" | "too-short" | "too-long" | "charset";

export const USERNAME_MIN = 3;
export const USERNAME_MAX = 30;
export const PASSWORD_MIN = 12;

export function checkUsername(value: string): UsernameProblem | null {
  const username = value.trim();
  if (username.length === 0) {
    return "empty";
  }
  if (username.length < USERNAME_MIN) {
    return "too-short";
  }
  if (username.length > USERNAME_MAX) {
    return "too-long";
  }
  // Lowercase because a Matrix localpart is case-sensitive and a homeserver
  // will happily create `Alice` beside `alice`; the wireframe forbids the
  // confusion rather than resolving it later.
  return /^[a-z0-9_-]+$/.test(username) ? null : "charset";
}

/** Why a password will not do. `weak` is the wireframe's floor, not taste. */
export type PasswordProblem = "empty" | "too-short" | "needs-letter-and-digit";

export function checkPassword(value: string): PasswordProblem | null {
  if (value.length === 0) {
    return "empty";
  }
  if (value.length < PASSWORD_MIN) {
    return "too-short";
  }
  if (!/[a-zA-Z]/.test(value) || !/[0-9]/.test(value)) {
    return "needs-letter-and-digit";
  }
  return null;
}

/**
 * The meter, 0 to 4. Deliberately crude: it counts length and the kinds of
 * character present, and it is feedback rather than a gate — [`checkPassword`]
 * is the gate. A real entropy estimator (zxcvbn and its dictionary) would cost
 * more than the whole app's JavaScript on the screens that carry 1.3 MB of
 * WebAssembly already.
 */
export function passwordStrength(value: string): 0 | 1 | 2 | 3 | 4 {
  if (value.length === 0) {
    return 0;
  }
  let score = 0;
  if (value.length >= PASSWORD_MIN) {
    score += 1;
  }
  if (value.length >= 16) {
    score += 1;
  }
  const kinds = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^a-zA-Z0-9]/].filter((kind) =>
    kind.test(value),
  ).length;
  if (kinds >= 3) {
    score += 1;
  }
  if (kinds === 4 && value.length >= 14) {
    score += 1;
  }
  return Math.min(score, 4) as 0 | 1 | 2 | 3 | 4;
}

/**
 * The localpart of a Matrix ID: `@alice:example.com` → `alice`. Used to
 * pre-fill the username when the Gateway has already told us who its owner is,
 * and to name the account in the recovery-key document.
 */
export function localpartOf(userId: string): string {
  const match = /^@([^:]+):/u.exec(userId.trim());
  return match?.[1] ?? "";
}

/**
 * The Matrix ID the deployment will create, for display only. The homeserver
 * decides the real one and answers with it.
 */
export function matrixIdFor(username: string, serverName: string): string {
  // The domain the user typed is where the client API answers, and it may
  // carry a port (`twalk.localhost:8009` on a tunnelled deployment). A
  // Matrix ID's domain is the **server name**, which is the host: building
  // `@michel:twalk.localhost:8009` produced a user the homeserver had never
  // heard of, and the recovery screen reported it as a refused password
  // (#96). Stripping the port is right whenever the port is only how the
  // API is reached; a deployment whose server name genuinely includes a
  // port must take it from the homeserver rather than from this input.
  return `@${username.trim()}:${serverNameOf(serverName)}`;
}

/** The server name in a typed domain: the host, without the API's port. */
export function serverNameOf(domain: string): string {
  const trimmed = domain.trim();
  const colon = trimmed.lastIndexOf(":");
  if (colon === -1) {
    return trimmed;
  }
  const after = trimmed.slice(colon + 1);
  return /^[0-9]+$/.test(after) ? trimmed.slice(0, colon) : trimmed;
}
