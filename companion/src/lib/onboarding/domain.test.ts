// Screen 1's "Continue" is disabled until this says yes, so what it accepts is
// the whole gate on the first thing a user types.

import { describe, expect, it } from "vitest";
import { get } from "svelte/store";

import {
  domain,
  homeserverBaseUrl,
  isValidDomain,
  normaliseDomain,
  rememberDomain,
  restoreDomain,
} from "./domain";

describe("normaliseDomain", () => {
  it("strips the decoration a user pastes", () => {
    expect(normaliseDomain("https://Example.COM/")).toBe("example.com");
    expect(normaliseDomain("  http://twalk.example.com:8443/setup?x=1 ")).toBe(
      "twalk.example.com",
    );
    expect(normaliseDomain("example.com.")).toBe("example.com");
  });

  it("is idempotent", () => {
    expect(normaliseDomain(normaliseDomain("HTTPS://a.B.fr/x"))).toBe("a.b.fr");
  });

  it("hands anything unparseable back for validation to reject", () => {
    expect(normaliseDomain("not a domain")).toBe("not a domain");
    expect(normaliseDomain("")).toBe("");
  });
});

describe("isValidDomain", () => {
  it("accepts the hostnames a deployment actually has", () => {
    expect(isValidDomain("example.com")).toBe(true);
    expect(isValidDomain("twalk.example.co.uk")).toBe(true);
    expect(isValidDomain("my-hub.example.fr")).toBe(true);
    // Through normalisation, so a pasted URL passes too.
    expect(isValidDomain("https://example.com/")).toBe(true);
  });

  it("refuses a single label that is not loopback", () => {
    // A LAN name is not a Twalk deployment, and it would send the user
    // into a homeserver probe that cannot succeed.
    expect(isValidDomain("twalk")).toBe(false);
    expect(isValidDomain("nas")).toBe(false);
  });

  it("refuses an IP address that is not loopback", () => {
    // No certificate, so no secure context, so no crypto (ADR 0014).
    expect(isValidDomain("192.168.1.10")).toBe(false);
    expect(isValidDomain("192.168.1.10:8008")).toBe(false);
  });

  it("accepts loopback, with its port", () => {
    // The exception, and the reason for it: a browser treats loopback as a
    // secure context without a certificate, which is what lets a developer
    // and the Playwright harness reach a deployment on this machine.
    // `*.localhost` resolves to loopback without DNS.
    expect(isValidDomain("localhost")).toBe(true);
    expect(isValidDomain("twalk.localhost:19148")).toBe(true);
    expect(isValidDomain("127.0.0.1:8008")).toBe(true);
  });

  it("refuses malformed labels", () => {
    expect(isValidDomain("")).toBe(false);
    expect(isValidDomain("exa mple.com")).toBe(false);
    expect(isValidDomain("-example.com")).toBe(false);
    expect(isValidDomain("example-.com")).toBe(false);
    expect(isValidDomain("example..com")).toBe(false);
    expect(isValidDomain("exa_mple.com")).toBe(false);
  });

  it("refuses a hostname longer than DNS allows", () => {
    expect(isValidDomain(`${"a".repeat(64)}.com`)).toBe(false);
    expect(isValidDomain(`${`${"a".repeat(50)}.`.repeat(6)}com`)).toBe(false);
  });
});

describe("homeserverBaseUrl", () => {
  it("is HTTPS for a real deployment", () => {
    expect(homeserverBaseUrl("example.com")).toBe("https://example.com");
    expect(homeserverBaseUrl("https://Example.COM/")).toBe(
      "https://example.com",
    );
  });

  it("is plain HTTP on loopback, which has no certificate to have", () => {
    expect(homeserverBaseUrl("twalk.localhost:19148")).toBe(
      "http://twalk.localhost:19148",
    );
    expect(homeserverBaseUrl("127.0.0.1:8008")).toBe("http://127.0.0.1:8008");
  });
});

describe("remembering the domain", () => {
  // `sessionStorage` is per tab, and /recover is a page a user reaches in a
  // new tab (or from a bookmark on the next day). With the domain in
  // `sessionStorage` that page had no domain, resolved an empty homeserver
  // URL, and failed with a raw `Failed to fetch` (#112). So the store this
  // uses is part of the behaviour, and is asserted rather than assumed.
  function withFakeWindow(
    run: (stores: { local: Storage; session: Storage }) => void,
  ): void {
    const make = (): Storage => {
      const held = new Map<string, string>();
      return {
        getItem: (key: string) => held.get(key) ?? null,
        setItem: (key: string, value: string) => void held.set(key, value),
        removeItem: (key: string) => void held.delete(key),
        clear: () => held.clear(),
        key: () => null,
        get length() {
          return held.size;
        },
      } as Storage;
    };
    const stores = { local: make(), session: make() };
    const previous = (globalThis as { window?: unknown }).window;
    (globalThis as { window?: unknown }).window = {
      localStorage: stores.local,
      sessionStorage: stores.session,
    };
    try {
      run(stores);
    } finally {
      (globalThis as { window?: unknown }).window = previous;
    }
  }

  it("survives a new tab, which is what a fresh sessionStorage is", () => {
    withFakeWindow((stores) => {
      rememberDomain("twalk.localhost:8009");
      expect(stores.local.getItem("twalk:domain")).toBe("twalk.localhost:8009");
      expect(
        stores.session.length,
        "nothing was put where a new tab cannot see it",
      ).toBe(0);

      // The new tab: a store it shares, and a session store it does not.
      domain.set("");
      restoreDomain();
      expect(get(domain)).toBe("twalk.localhost:8009");
    });
  });

  it("refuses a remembered value that is no longer a valid domain", () => {
    withFakeWindow((stores) => {
      stores.local.setItem("twalk:domain", "not a domain at all");
      domain.set("");
      restoreDomain();
      expect(get(domain)).toBe("");
    });
  });
});
