---
description: Investigate supplied CI failures and prepare a bounded repair without bypassing checks.
---

# Fix CI with Foundry

Apply the [Foundry coordination skill](../skills/foundry/SKILL.md).

Identify the repository workspace, failing run, revision, job and first actionable
failure. Use CI output already available through authorized client tools or supplied
by the user. Foundry's five MCP tools do not fetch hosted CI logs. If those facts
are missing, request the specific log excerpt or local reproduction.

Distinguish test failures, build errors, infrastructure faults and missing setup.
Prepare a task containing the failing command, relevant environment constraints,
observed output and the required validation. Keep secrets out of pasted logs.
Preserve required checks; do not remove tests, lower thresholds, edit branch
protections or retry paid infrastructure to produce an apparent green result.

Use `session_create` for an authorized new repair task or `session_send` for a
selected existing task. These persist instructions only. Execute through a
pre-existing configured CLI only when execution is part of the request; report
which path was actually used.

Report local validation separately from hosted CI. A local pass does not establish
a remote run passed, and a prior green run does not validate a new revision.
Publishing a patch or rerunning hosted jobs needs an available authorized tool;
this workflow supplies neither. Keep the actual run URL when supplied, but never
construct a dashboard link or claim a queued repair has fixed CI.
