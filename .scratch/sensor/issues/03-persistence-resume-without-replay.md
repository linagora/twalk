# 03: Persistence — resume without replay

**What to build:** an operator can restart, upgrade or crash the Sensor without the bus noticing. The sync token and the crypto store are persisted on a volume, so the process resumes where it left off instead of re-synchronizing from scratch. Messages sent while the Sensor was down are delivered after restart — exactly once, thanks to deterministic ids and JetStream deduplication working together.

**Blocked by:** 02 — Walking skeleton.

**Status:** ready-for-agent

- [ ] Sync token and crypto store survive process restarts on a volume
- [ ] Restarting mid-traffic produces no duplicate CloudEvents ids on the bus
- [ ] Messages sent during a downtime window are delivered after restart, exactly once
- [ ] A fresh start with an empty store still works (no corruption path)
