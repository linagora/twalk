# Issue tracker

The project's issue tracker is **GitHub Issues on [linagora/twalk](https://github.com/linagora/twalk/issues)**.

## Labels

- `ready-for-agent` — a self-contained ticket an agent can grab; all blockers declared.
- `needs-triage` — a raw incoming issue (bug report, feature request), not yet triaged.
- `spec` — a parent spec issue holding a full feature spec.

## Conventions

- Specs are parent issues labelled `spec`; tickets reference them in a "Part of #N" **Parent** section.
- Blocking edges live twice: in the ticket's **Blocked by** body section (human-readable) and as native GitHub issue dependencies (machine-checkable). The gh CLI does not expose dependencies; set them with `gh api graphql -f query='mutation { addBlockedBy(input: {issueId: "<node-id>", blockingIssueId: "<node-id>"}) { clientMutationId } }'` (node ids via `gh issue view N --json id`).
- Read an issue: `gh issue view N --repo linagora/twalk --json title,body,labels`.
- The frontier: any open `ready-for-agent` issue whose native `blockedBy` nodes are all closed.
- `.scratch/<feature>/` keeps local mirrors of specs and tickets for offline reading; GitHub is canonical when they disagree.
