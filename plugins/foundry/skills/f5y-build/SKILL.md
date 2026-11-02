---
name: f5y-build
description: Turn a requested change into a concrete Foundry task with acceptance checks.
---

# Build with Foundry

Apply the [Foundry coordination skill](../foundry/SKILL.md).

Extract the intended behavior, workspace, affected interfaces and acceptance
checks from the request. Read available project instructions and existing design
context. Clarify only a missing choice that would materially change the result.
Preserve other contributors' work and identify files outside the task's scope.

Write a task prompt containing the desired behavior, relevant paths, constraints,
validation commands and expected deliverables. Require the executing agent to
inspect existing code before editing and report what it actually tested. Do not
turn a feature request into permission to publish, merge or change credentials.

For an authorized queue submission, use `session_create` with the explicit local
directory and stable idempotency key. For additional work in a deliberately selected
session, use `session_send` with a new message ID. Report queue acceptance as such.
If the user requested execution and an explicit configured CLI is available, use
that separate execution path; do not create a duplicate queued task first.

Inspect the resulting session when execution exists. Summarize changed behavior,
checks, limitations and remaining work. Do not describe a queued prompt as a built
feature, a delivered instruction as completion, or an exit without its outcome as
success. Share a PR link only if an actual authorized operation produced one.
