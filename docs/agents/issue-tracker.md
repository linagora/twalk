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

## Workflow: one PR per ticket, linked to its issue

Work never lands on `main` without a pull request. Each ticket is implemented on its own branch and merged through a PR whose body contains `Closes #N` — that links the PR to the issue (visible in the issue's Development section) and closes the issue automatically on merge.

- Branch from an up-to-date `main`; push the branch; open the PR early (`gh pr create`), titled after the ticket.
- The PR body: summary + `Closes #N` + test evidence (suite results).
- The two-axis review happens on the PR diff before merge; merge only with a green full suite.
- Exception (bootstrap history): tickets 01–11 of the Sensor were merged locally before this rule existed; their issues carry the merge-commit reference in a closing comment instead.

## Tracker operations

For skills that read or write the tracker, the `gh` CLI is the interface; run inside a clone and it infers the repo from `git remote -v`.

- **Create**: `gh issue create --title "..." --body-file <file>` (a heredoc into a file keeps multi-line bodies readable).
- **Read**: `gh issue view <n> --comments`, or `--json title,body,labels` for the parts you need.
- **List**: `gh issue list --state open --json number,title,labels --jq '.[]|"\(.number)\t\([.labels[].name]|join(","))\t\(.title)"'`, with `--label` / `--state` filters.
- **Comment**: `gh issue comment <n> --body "..."`
- **Label**: `gh issue edit <n> --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <n> --comment "..."` (a ticket closed by a merged PR needs no manual close — `Closes #N` does it).

"Publish to the issue tracker" means create a GitHub issue; "fetch the relevant ticket" means `gh issue view <n> --comments`.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; the triage skill reads this flag.)_

## Wayfinding operations

For skills that navigate a feature as a map plus tickets, reuse this repo's existing shapes rather than parallel ones:

- **Map**: the parent `spec` issue (see the conventions above), holding the spec body.
- **Child ticket**: an issue with a `Part of #<map>` **Parent** section and the `ready-for-agent` label once it is self-contained.
- **Blocking**: native GitHub issue dependencies, set with the `addBlockedBy` GraphQL mutation documented above, mirrored by a **Blocked by** body section. A ticket is unblocked when every blocker is closed.
- **Frontier**: any open `ready-for-agent` issue whose `blockedBy` nodes are all closed and which nobody is working on.
