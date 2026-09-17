# Domain docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root: the domain glossary, and the authority on which word to use.
- **`docs/architecture/adr/`**: the ADRs. Read the ones touching the area you are about to work in. (Note the path — decisions live under `docs/architecture/adr/`, not `docs/adr/`. ADRs 0005–0009 are written; 0001–0004 are referenced from the docs but not yet written.)
- **`docs/architecture/roadmap.md`** when the work spans components: it maps the product milestones to the lots, with their specs and build order.

If a file doesn't exist, **proceed silently**. Don't flag its absence or suggest creating it upfront. The domain-modeling skill creates them lazily, when terms or decisions actually get resolved.

## File structure

Single-context repo: one glossary and one ADR directory for the whole monorepo, whose components (`sensor/`, `hermes/`, `companion/`, `companion-gateway/`, …) share the same domain language by design.

```
/
├── CONTEXT.md
├── docs/architecture/
│   ├── adr/
│   │   ├── 0005-network-channel-and-bridge.md
│   │   └── …
│   └── roadmap.md
├── sensor/
├── hermes/
└── …
```

There is no `CONTEXT-MAP.md`: if a component ever needs its own glossary, add one and introduce the map then.

## Use the glossary's vocabulary

When your output names a domain concept — an issue title, a refactor proposal, a hypothesis, a test name, a log message — use the term as defined in `CONTEXT.md`. The vocabulary is enforced, not advisory (see the code style section of `AGENTS.md`): **network** (never "channel" outside user-facing copy, never a bridge name like `gmessages`), **persona** (never "bot" for agent identities; test Matrix users standing in for bridges are "test bots"), **the Companion** for the PWA only, **Companion Gateway** always in full.

If the concept you need isn't in the glossary, that's a signal: either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for the domain-modeling skill).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR 0006 (consent state owned by the Companion Gateway), but worth reopening because…_
