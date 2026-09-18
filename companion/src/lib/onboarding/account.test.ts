// Screen 2's first step will not call the Gateway until these say yes, so what
// they accept is the whole gate on the account the deployment gets — once,
// for ever.

import { describe, expect, it } from "vitest";

import {
  checkPassword,
  checkUsername,
  localpartOf,
  matrixIdFor,
  passwordStrength,
} from "./account";

describe("checkUsername", () => {
  it("accepts the wireframe’s alphabet", () => {
    expect(checkUsername("michel")).toBeNull();
    expect(checkUsername("mm_2026")).toBeNull();
    expect(checkUsername("a-b-c")).toBeNull();
  });

  it("refuses what the wireframe’s rules refuse", () => {
    expect(checkUsername("")).toBe("empty");
    expect(checkUsername("ab")).toBe("too-short");
    expect(checkUsername("a".repeat(31))).toBe("too-long");
    // Upper case is the interesting one: a Matrix localpart is
    // case-sensitive, so `Michel` and `michel` would be two accounts.
    expect(checkUsername("Michel")).toBe("charset");
    expect(checkUsername("michel.maudet")).toBe("charset");
    expect(checkUsername("michel maudet")).toBe("charset");
  });
});

describe("checkPassword", () => {
  it("accepts twelve characters with a letter and a digit", () => {
    expect(checkPassword("correct-horse1")).toBeNull();
  });

  it("refuses what the wireframe’s floor refuses", () => {
    expect(checkPassword("")).toBe("empty");
    expect(checkPassword("short1")).toBe("too-short");
    expect(checkPassword("alllettersonly")).toBe("needs-letter-and-digit");
    expect(checkPassword("123456789012")).toBe("needs-letter-and-digit");
  });
});

describe("passwordStrength", () => {
  it("is 0 for nothing and rises with length and variety", () => {
    expect(passwordStrength("")).toBe(0);
    expect(passwordStrength("short")).toBe(0);
    expect(passwordStrength("twelvechars1")).toBeGreaterThan(0);
    expect(passwordStrength("Correct-Horse-Battery-9")).toBe(4);
  });

  it("never exceeds the meter it draws", () => {
    expect(passwordStrength("x".repeat(200))).toBeLessThanOrEqual(4);
  });
});

describe("Matrix IDs", () => {
  it("reads a localpart back out", () => {
    expect(localpartOf("@michel:example.com")).toBe("michel");
    expect(localpartOf("not a matrix id")).toBe("");
  });

  it("spells the one the homeserver will create", () => {
    expect(matrixIdFor("michel", "example.com")).toBe("@michel:example.com");
    // The typed domain may carry the API's port; a Matrix ID's domain is
    // the server name, so the port is not part of it (#96).
    expect(matrixIdFor("michel", "twalk.localhost:8009")).toBe(
      "@michel:twalk.localhost",
    );
    expect(matrixIdFor("michel", "127.0.0.1:8008")).toBe("@michel:127.0.0.1");
  });
});
