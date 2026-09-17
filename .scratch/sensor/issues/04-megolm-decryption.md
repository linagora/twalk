# 04: Megolm decryption

**GitHub:** [linagora/twalk#5](https://github.com/linagora/twalk/issues/5) — canonical on the tracker; this file is the local mirror.

**What to build:** the Sensor works in real portal rooms, which are end-to-end encrypted. The operator bootstraps the Sensor's cryptographic identity from the recovery key saved during onboarding; the Sensor restores what it can and cross-signs bridge puppet devices as far as the bridges allow. A message sent by a bot in an encrypted room arrives decrypted and schema-valid on the bus. A room that cannot be decrypted (missing keys, poisoned session) is logged, counted and skipped — it never stalls the other rooms. All cryptography is delegated to the SDK's crypto crate; the Sensor implements none itself.

**Blocked by:** 02 — Walking skeleton.

**Status:** done

- [x] The Sensor bootstraps its cryptographic identity from the operator's recovery key
- [x] A message sent in an encrypted room arrives decrypted and schema-valid on the bus
- [x] An undecryptable room is logged, counted and skipped while other rooms keep flowing
- [x] No cryptographic primitive is implemented in the Sensor codebase
