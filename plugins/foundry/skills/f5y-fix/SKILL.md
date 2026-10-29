---
name: f5y-fix
description: Prepare and track a focused defect fix with a reproducible failure and regression check.
---

# Fix with Foundry

Apply the [Foundry coordination skill](../foundry/SKILL.md).

Collect expected behavior, observed behavior, a reproduction and the relevant
workspace or session. Inspect available logs and code before requesting more data.
Keep private values out of the task. Separate a confirmed failure from a hypothesis.

Describe the smallest useful reproduction, affected paths and required regression
check in the task prompt. Require the executing agent to identify the cause before
changing behavior, preserve unrelated edits and validate the original failing case.
Do not hide a defect by disabling checks, swallowing errors or widening permissions.

Choose queueing or execution before submitting work. Queue an authorized new task
with `session_create`, or add a distinct follow-up to a deliberately selected
session with `session_send`. Preserve IDs for retries. If fresh execution was
requested and a configured CLI is available, use that path directly without
creating a duplicate queued task first. Submission alone runs no fix. If a previous
attempt had uncertain effects, inspect its state rather than automatically replaying
it or starting a replacement.

Report the actual state and, once evidence exists, the defect addressed, modified
behavior and regression result. If the task remains queued, blocked or suspended,
state that directly. A proposed patch or successful queue response is not a verified
fix, and cancellation does not undo changes already made.
