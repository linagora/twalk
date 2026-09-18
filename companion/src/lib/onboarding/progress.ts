// What screen 1 hands to screen 2, and what screen 2 hands to the rest of the
// app. Two stores with two very different lifetimes, and the difference is the
// point.
//
// **The homeserver** is a public URL the user could read off their own DNS. It
// is remembered in `localStorage` beside the domain (`./domain.ts`) — per
// browser rather than per tab, since /recover is reachable in a new tab and
// failed on an empty homeserver URL without it (#112) — so that
// reloading screen 2 does not send the user back to type the domain again, and
// it dies with the tab.
//
// **The Matrix session** — the access token the Gateway relayed back from the
// registration — is in memory and nowhere else. Spec #65: nothing sensitive in
// browser storage. A reload therefore loses it, and that is correct: what
// keeps the user signed in across a reload is the Gateway's own `HttpOnly`
// cookie plus the crypto store in IndexedDB, neither of which this app can
// read. The Matrix token is needed again for the network screens (#68), which
// re-obtain it rather than find it lying about.
//
// Nothing here records *progress* as a flag. Spec #65 derives that from what
// exists on the Gateway (`./deployment.ts`) and from whether the crypto store
// is present (`$lib/crypto/store.ts`), because a flag in browser storage is a
// second source of truth that can be wrong.

import { writable } from "svelte/store";

import type { MatrixSession } from "$lib/crypto/bootstrap";

const HOMESERVER_KEY = "twalk:homeserver";

/** The homeserver base URL screen 1 resolved, or `''` before it did. */
export const homeserver = writable<string>("");

/** Remembers it for the rest of this tab's life. Browser-only. */
export function rememberHomeserver(baseUrl: string): void {
  homeserver.set(baseUrl);
  try {
    window.localStorage.setItem(HOMESERVER_KEY, baseUrl);
  } catch {
    // Storage can be switched off; screen 2 re-resolves it.
  }
}

/** Reads it back. Browser-only; called from a component's `onMount`. */
export function restoreHomeserver(): string {
  try {
    const stored = window.localStorage.getItem(HOMESERVER_KEY);
    if (stored !== null && stored.length > 0) {
      homeserver.set(stored);
      return stored;
    }
  } catch {
    // See above.
  }
  return "";
}

/**
 * The live Matrix session: in memory, for this page's life. `null` before an
 * account exists and after a reload.
 */
export const matrixSession = writable<MatrixSession | null>(null);
