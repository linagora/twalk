Closes #

<!--
`Closes #N` above, as plain text on its own line. Never inside backticks —
GitHub does not link it, and the issue stays open after the merge.
-->

## Summary

<!--
What changed and why, in this project's register: the decision, not the diff.
`docs/architecture/adr/` is where a decision that is hard to reverse, surprising
without its context, and the result of a real trade-off goes.
-->

## Things to argue with

<!--
The alternatives considered and why they lost. A reviewer's best comment is one
that disputes a decision on its merits, and they can only do that if the
decision is stated.
-->

## Evidence

<!--
The actual output, not a claim. `verified` on this pull request covers the
required suites; if your change touches `deploy/docker-compose/` or the
reference deployment, add the `ci:deployment` label so the advisory suites run
too, or paste the run.

If a suite is red, say whether it is the change or one of the known flakes
(#148, #186) — and if you re-ran it, say so and say how many times. A red check
people learn to ignore is worse than no check.
-->
