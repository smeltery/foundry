---
description: Prepare and track a focused defect fix with a reproducible failure and regression check.
---

# Fix with Foundry

Apply the [Foundry coordination skill](../skills/foundry/SKILL.md).

Collect expected behavior, observed behavior, a reproduction and the relevant
workspace or session. Inspect available logs and code before requesting more data.
Keep private values out of the task. Separate a confirmed failure from a hypothesis.

Describe the smallest useful reproduction, affected paths and required regression
check in the task prompt. Require the executing agent to identify the cause before
changing behavior, preserve unrelated edits and validate the original failing case.
Do not hide a defect by disabling checks, swallowing errors or widening permissions.

Queue the requested fix with `session_create`, or add a distinct follow-up to the
selected session with `session_send`. Preserve IDs for retries. Submission alone
runs no fix; an already configured CLI execution path is separate. If a previous
attempt had uncertain effects, inspect its state rather than automatically replaying
it or starting a replacement.

Report the actual state and, once evidence exists, the defect addressed, modified
behavior and regression result. If the task remains queued, blocked or suspended,
state that directly. A proposed patch or successful queue response is not a verified
fix, and cancellation does not undo changes already made.
