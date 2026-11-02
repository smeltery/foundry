---
name: f5y-ask
description: Answer a scoped question using available evidence or queue an explicit Foundry investigation.
---

# Ask Foundry

Apply the [Foundry coordination skill](../foundry/SKILL.md).

Identify the question and the evidence needed to answer it. Use supplied files,
logs and session IDs before asking for more context. For a question about ongoing
work, fetch the selected session with `session_get`; do not create another task.
Distinguish stored task output from an independently verified answer.

For an explicitly requested investigation through Foundry, compose a prompt with
the question, absolute workspace directory, relevant paths and desired answer
format. Keep the investigation read-only unless the user asked for changes.
Use `session_create` with a stable idempotency key, or `session_send` for a selected
existing session with a new message ID. Neither operation executes the question.

If the user needs an immediate answer and execution is unavailable, answer only
what the available evidence supports and identify the remaining gap. Do not claim
repository search, remote research or provider execution from queue submission.

Return the answer with its evidence, or the actual queued session/instruction ID
and unconfirmed execution state. Name unresolved questions without inventing facts,
links or future notification promises.
