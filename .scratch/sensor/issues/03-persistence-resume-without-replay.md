# 03: Persistence — resume without replay

**GitHub:** [linagora/twalk#4](https://github.com/linagora/twalk/issues/4) — canonical on the tracker; this file is the local mirror.

**What to build:** an operator can restart, upgrade or crash the Sensor without the bus noticing. The sync token and the crypto store are persisted on a volume, so the process resumes where it left off instead of re-synchronizing from scratch. Messages sent while the Sensor was down are delivered after restart — exactly once, thanks to deterministic ids and JetStream deduplication working together.

**Blocked by:** 02 — Walking skeleton.

**Status:** done

- [x] Sync token and crypto store survive process restarts on a volume
- [x] Restarting mid-traffic produces no duplicate CloudEvents ids on the bus
- [x] Messages sent during a downtime window are delivered after restart, exactly once
- [x] A fresh start with an empty store still works (no corruption path)
