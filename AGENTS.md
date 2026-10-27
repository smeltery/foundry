# Foundry contributor instructions

## Workflow

- Read the assigned Linear issue and project requirements before editing.
- Keep upstream research and source mapping outside this repository.
- Coordinate shared manifests, lockfiles and interfaces with the integration owner.
- Do not overwrite another agent's edits. Use focused branches and explicit file ownership.
- Follow the dependency graph; unavailable functionality is not completed work.
- Keep Linear status current: move assigned work to In Progress when execution starts,
  to In Review when its implementation and local checks are ready, and to Done only
  when its acceptance criteria and required delivery checks pass. Record evidence
  and remaining blockers with each transition; progress notes do not replace status updates.

## Code and checks

- Use explicit error handling and cohesive, descriptive modules.
- Preserve verified public behavior and meaningful tests during refactors.
- Use the pinned Flox environment and shared check commands once available.
- Use Bun for compatible JavaScript tooling and Cargo for Rust.
- Keep code within reviewed file and component budgets without minification or feature removal.
- Validate Markdown, links and diagrams through the shared checks.
- Keep docs focused on verified capabilities and known limitations.
- Scan tracked content and release outputs against the externally supplied naming policy.

## Completion

- Run relevant local checks, push the branch, and monitor applicable CI until green.
- Record the final commit and CI run links in Linear before marking implementation complete.
- Keep release blocked on unresolved required checks or import constraints.
- Never commit credentials, environment secrets or third-party content lacking a compatible import decision.

## Git

- Use conventional commits with a first line at most 72 characters.
- Do not add co-author or generated-by attribution footers.
- Never force-push main or master.
