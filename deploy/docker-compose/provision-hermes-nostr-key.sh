#!/usr/bin/env bash
# Give Hermes a Nostr signing key of its own, for Buzz.
#
#   ./provision-hermes-nostr-key.sh [hermes-env-file]   default: ~/.hermes-twalk/.env
#   ./provision-hermes-nostr-key.sh --self-test
#
# Hermes signs every Buzz event with a Nostr key, and the relay
# (`restricted_writes: true`) refuses a key it was not told to accept. Nothing
# said how that key is made or where it goes, so the next operator would
# either reinvent a secp256k1 derivation or, worse, reuse the owner's key —
# which is what ADR 0019's disclosure exists to prevent: a message signed with
# the owner's key is indistinguishable from the owner writing it (issue #239).
#
# This script is on the right side of ADR 0032's line. The Companion configures
# the seam to Hermes and never Hermes's own key, because that would mean Twalk
# holding a Nostr private key and writing files on a host it may not own; an
# operator-run script that writes into Hermes's own env file is not Twalk
# holding anything.
#
# What it does, and does not do:
#
# - Generates 32 random bytes from the kernel and writes them as
#   `BUZZ_PRIVATE_KEY=<hex>` into Hermes's env file, mode 0600. Once: a second
#   run leaves an existing key untouched and reprints its public half, because
#   a key that rotates is an identity the relay no longer knows.
# - Prints the **public** half, hex and `npub`, and says what to do with it —
#   the relay has to be told to accept it (`./run.sh add-member <hex>` in
#   Buzz's compose bundle), and the private half is never printed.
# - Never carries the private key in any argv: `ps` shows a command line to
#   every user on the host. The derivation reads it from stdin, the same rule
#   `provision-owner-device.sh` follows for the password.
#
# The derivation is pure Python 3 (secp256k1 point multiplication and bech32),
# because no host has a Nostr tool by default and the one dependency this
# script may assume is the interpreter the deployment's own tooling already
# needs. `--self-test` checks it against the curve's generator: private key 1
# derives to Gx.
set -euo pipefail

# The derivation, as a program handed to the interpreter on its command line —
# which is fine, it is not secret — so that stdin stays free for the key.
DERIVE_PROGRAM=$(cat <<'PY'
import sys

P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


def add(p, q):
    if p is None:
        return q
    if q is None:
        return p
    (x1, y1), (x2, y2) = p, q
    if x1 == x2 and (y1 + y2) % P == 0:
        return None
    if p == q:
        lam = (3 * x1 * x1) * pow(2 * y1, P - 2, P) % P
    else:
        lam = (y2 - y1) * pow(x2 - x1, P - 2, P) % P
    x3 = (lam * lam - x1 - x2) % P
    return (x3, (lam * (x1 - x3) - y1) % P)


def mul(k, point):
    result = None
    while k:
        if k & 1:
            result = add(result, point)
        point = add(point, point)
        k >>= 1
    return result


CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"


def bech32_polymod(values):
    gen = [0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3]
    chk = 1
    for value in values:
        top = chk >> 25
        chk = ((chk & 0x1FFFFFF) << 5) ^ value
        for i in range(5):
            chk ^= gen[i] if (top >> i) & 1 else 0
    return chk


def bech32_encode(hrp, data):
    hrp_expanded = [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp]
    polymod = bech32_polymod(hrp_expanded + data + [0] * 6) ^ 1
    checksum = [(polymod >> 5 * (5 - i)) & 31 for i in range(6)]
    return hrp + "1" + "".join(CHARSET[d] for d in data + checksum)


def to_5bit(raw):
    acc, bits, out = 0, 0, []
    for byte in raw:
        acc = (acc << 8) | byte
        bits += 8
        while bits >= 5:
            bits -= 5
            out.append((acc >> bits) & 31)
    if bits:
        out.append((acc << (5 - bits)) & 31)
    return out


secret = int(sys.stdin.read().strip(), 16)
if not 1 <= secret < N:
    sys.exit("the private key is not a valid secp256k1 scalar")
x, _y = mul(secret, G)
pubkey = x.to_bytes(32, "big")
print(pubkey.hex(), bech32_encode("npub", to_5bit(pubkey)))
PY
)

derive() {
	# Reads one hex private key on stdin; prints `<hex pubkey> <npub>`.
	python3 -c "$DERIVE_PROGRAM"
}

self_test() {
	local expected_hex=79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798
	local expected_npub=npub10xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqpkge6d
	local got
	got="$(printf '%064x' 1 | derive)"
	if [ "$got" != "$expected_hex $expected_npub" ]; then
		echo "self-test failed: private key 1 derived to '$got'" >&2
		exit 1
	fi
	echo "self-test ok: private key 1 derives to Gx and $expected_npub"
}

if [ "${1:-}" = "--self-test" ]; then
	self_test
	exit 0
fi

ENV_FILE="${1:-${HERMES_HOME:-$HOME/.hermes-twalk}/.env}"
mkdir -p "$(dirname "$ENV_FILE")"
umask 077
touch "$ENV_FILE"
chmod 0600 "$ENV_FILE"

if grep -qE '^BUZZ_PRIVATE_KEY=' "$ENV_FILE"; then
	echo "Hermes already has a Buzz key in $ENV_FILE; leaving it untouched."
	existing="$(sed -nE 's/^BUZZ_PRIVATE_KEY=//p' "$ENV_FILE" | head -1 | tr -d "\"'")"
	case "$existing" in
	nsec1*)
		echo "It is stored as an nsec, which this script does not decode; the public half is what" >&2
		echo "Buzz Desktop or the buzz CLI shows for that key." >&2
		exit 1
		;;
	esac
	read -r pubkey npub < <(printf '%s' "$existing" | derive)
else
	# The kernel's randomness, straight into the file: the private half is
	# never in a variable this shell could leak into a log or a command line.
	{
		printf '\n# The Nostr key Hermes signs Buzz events with — its own identity, never the\n'
		printf '# owner'"'"'s (issue #239, ADR 0019, ADR 0032). Written by provision-hermes-nostr-key.sh.\n'
		printf 'BUZZ_PRIVATE_KEY=%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
	} >> "$ENV_FILE"
	echo "Wrote Hermes's Buzz key into $ENV_FILE (mode 0600)."
	read -r pubkey npub < <(sed -nE 's/^BUZZ_PRIVATE_KEY=//p' "$ENV_FILE" | head -1 | derive)
fi

# The public half beside the private one, so the channel script and an
# operator reading the file can find it without deriving it again. Public, so
# a stale line is corrected rather than kept.
if grep -qE '^BUZZ_PUBLIC_KEY=' "$ENV_FILE"; then
	sed -i -E "s/^BUZZ_PUBLIC_KEY=.*/BUZZ_PUBLIC_KEY=$pubkey/" "$ENV_FILE"
else
	printf 'BUZZ_PUBLIC_KEY=%s\n' "$pubkey" >> "$ENV_FILE"
fi

cat <<EOF

Hermes's Buzz identity (public half — the private half stays in $ENV_FILE):
  hex:  $pubkey
  npub: $npub

The relay refuses a key it was not told to accept (restricted_writes). Tell it:
  cd <buzz>/deploy/compose && ./run.sh add-member $pubkey
Then add this pubkey to the channels Hermes should read, as the owner.
EOF
