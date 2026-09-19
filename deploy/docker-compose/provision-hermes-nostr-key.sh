#!/usr/bin/env bash
# Generate the Nostr identity the persona runtime signs Buzz events with.
#
# This key is **Hermes's own**, never the owner's. A message signed with the
# owner's key would be indistinguishable from the owner writing it, which is
# exactly what ADR 0019's disclosure exists to prevent: the contact — and the
# owner reading their own approval queue — must be able to tell which of the
# two wrote a sentence.
#
#   ./provision-hermes-nostr-key.sh [env-file]     default: ~/.hermes-twalk/.env
#
# The private key is written straight into the env file (mode 0600) and is
# never printed, logged or echoed. Only the public key reaches stdout, because
# that is the half you have to hand out: the relay advertises
# `restricted_writes: true`, so it refuses writes from a key it has not been
# told to accept.
#
# Idempotent: an env file that already carries a key is left untouched, and
# its public half is recomputed and printed. Rotating is deliberate — remove
# the BUZZ_PRIVATE_KEY line yourself first — because a new key is a new author
# and the relay will not know it.
set -euo pipefail

ENV_FILE="${1:-$HOME/.hermes-twalk/.env}"
umask 077
mkdir -p "$(dirname "$ENV_FILE")"
touch "$ENV_FILE"
chmod 600 "$ENV_FILE"

# Derive the x-only public key (NIP-01: 32 bytes, the X coordinate) and the
# bech32 npub (NIP-19) from a private key. The key travels in the interpreter's
# environment, not on its command line: an argument is in `ps` for every user
# on the host for as long as the derivation runs, the environment is readable
# by its owner alone. (Stdin is taken by the program itself.) Nothing here
# writes the private key anywhere but the env file.
derive() {
  NOSTR_PRIVATE_KEY_HEX="$1" python3 - <<'PY'
import os, sys
priv = os.environ.pop("NOSTR_PRIVATE_KEY_HEX", "").strip()
if len(priv) != 64 or not all(c in "0123456789abcdef" for c in priv.lower()):
    sys.exit("private key is not 64 hex characters")
# secp256k1 scalar multiplication, no third-party library.
P  = 2**256 - 2**32 - 977
N  = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
Gx = 0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798
Gy = 0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8
d = int(priv, 16)
if not 1 <= d < N:
    sys.exit("private key is out of range for secp256k1")
def add(p, q):
    if p is None: return q
    if q is None: return p
    if p[0] == q[0] and (p[1] + q[1]) % P == 0: return None
    if p == q: l = (3 * p[0] * p[0]) * pow(2 * p[1], P - 2, P) % P
    else:      l = (q[1] - p[1]) * pow(q[0] - p[0], P - 2, P) % P
    x = (l * l - p[0] - q[0]) % P
    return (x, (l * (p[0] - x) - p[1]) % P)
r, acc = None, (Gx, Gy)
while d:
    if d & 1: r = add(r, acc)
    acc = add(acc, acc); d >>= 1
xonly = "%064x" % r[0]
# bech32 (NIP-19 uses bech32, not bech32m)
CS = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
def polymod(v):
    gen = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]
    c = 1
    for d_ in v:
        b = c >> 25
        c = ((c & 0x1ffffff) << 5) ^ d_
        for i in range(5):
            if (b >> i) & 1: c ^= gen[i]
    return c
def convert(data):
    acc = bits = 0; out = []
    for b in data:
        acc = (acc << 8) | b; bits += 8
        while bits >= 5:
            bits -= 5; out.append((acc >> bits) & 31)
    if bits: out.append((acc << (5 - bits)) & 31)
    return out
hrp = "npub"
data = convert(bytes.fromhex(xonly))
exp = [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp]
chk = polymod(exp + data + [0, 0, 0, 0, 0, 0]) ^ 1
npub = hrp + "1" + "".join(CS[c] for c in data) + "".join(CS[(chk >> 5 * (5 - i)) & 31] for i in range(6))
print(xonly); print(npub)
PY
}

existing="$(grep -E '^BUZZ_PRIVATE_KEY=' "$ENV_FILE" | tail -1 | cut -d= -f2- || true)"
if [ -n "$existing" ]; then
  echo "a key is already present in $ENV_FILE — leaving it alone" >&2
  priv="$existing"
else
  # A Nostr private key is 32 random bytes that form a valid secp256k1 scalar.
  # `openssl rand` is the CSPRNG; `derive` rejects anything outside [1, n-1],
  # so the loop is a formality that costs nothing and removes an assumption.
  priv=""
  for _ in 1 2 3; do
    candidate="$(openssl rand -hex 32)"
    if [ "${#candidate}" -eq 64 ] && derive "$candidate" >/dev/null 2>&1; then
      priv="$candidate"; break
    fi
  done
  unset candidate
  if [ -z "$priv" ]; then
    echo "could not generate a valid secp256k1 key" >&2; exit 1
  fi
  printf 'BUZZ_PRIVATE_KEY=%s\n' "$priv" >> "$ENV_FILE"
  echo "wrote a new private key to $ENV_FILE (mode 0600, never printed)" >&2
fi

out="$(derive "$priv")"
unset priv
echo
echo "Hermes's public key — this is the half you hand out:"
echo "  hex   $(echo "$out" | sed -n 1p)      <- buzz channels add-member --pubkey"
echo "  npub  $(echo "$out" | sed -n 2p)"
echo
echo "Next: authorise that key on the relay (it advertises restricted_writes: true),"
echo "otherwise everything Hermes sends is refused."
