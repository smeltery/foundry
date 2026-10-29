---
description: Scope an evidence-based review of supplied changes without implying a hosted review service.
---

# Review with Foundry

Apply the [Foundry coordination skill](../skills/foundry/SKILL.md).

Identify the exact changes to review: local diff, explicit revision range or
user-supplied PR context. Preserve the base and head identifiers. A PR URL alone
does not grant access or supply the diff; use an existing authorized repository
tool or request the missing patch. Do not claim Foundry fetched it.

Prepare a read-only review task covering correctness, regressions, security,
lifecycle behavior and missing validation relevant to those changes. Require each
finding to identify a concrete trigger, impact and location. Separate reproducible
defects from suggestions; avoid speculative redesign or edits to the reviewed work.

Queue an explicitly requested review with `session_create` or a selected session's
`session_send`. This is an ordinary local task, not a hosted PR-review trigger.
No review-service listing or posting API is provided. If current evidence already
supports a direct review response, provide that response without creating busywork.

When results are available, present findings by impact with file locations and
supporting checks. If none are found, state the scope inspected and validation
limits. Do not promise absence of defects. Label queued reviews as unexecuted and
do not report approval, posted comments or a PR status change without evidence from
an actual authorized operation.
