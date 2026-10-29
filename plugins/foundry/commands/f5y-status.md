---
description: Inspect Foundry session state while keeping queueing, delivery and completion distinct.
---

# Foundry status

Apply the [Foundry coordination skill](../skills/foundry/SKILL.md).

For a supplied ID, call `session_get` directly. Otherwise call `session_list` and
identify the intended session by prompt and directory. The list is bounded and
has no pagination argument; do not infer a known session is missing from its
absence there. Ask if the intended session is ambiguous.

Report the actual session ID, lifecycle, separate outcome, active attempt and
available result. A queued record has not started execution. Running is not
completion. An exit requires checking success, failure or action-required; error
and suspended do not carry a successful outcome.

For an instruction submission, preserve its message ID and returned delivery and
completion fields. This MCP surface cannot enumerate messages or conversation
history. Do not infer a specific instruction completed from the parent session's
state alone. State when only an earlier submission response is available.

If monitoring is requested, fetch state at bounded intervals and report the last
observation if monitoring stops. An available CLI watch can show stored events;
it does not launch work. Do not invent progress percentages, event cursors,
subscription callbacks or hosted session URLs.

Status is read-only. Do not create work, send a nudge, resume, cancel or retry a
session merely because it appears idle. If the user also requests one of those
actions, handle it explicitly with the coordination skill's supported operations
and report that action separately from observed state.
