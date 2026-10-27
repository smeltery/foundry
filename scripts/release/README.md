# Release planner

`planner.py` provides a Python 3.9+ standard-library API with no filesystem,
network, process, or GitHub operations. Call `plan_release(Request(...))` to
obtain an immutable `Plan`. Inputs are typed dataclass snapshots, not raw JSON.
An integration adapter must validate its API responses before constructing them.
Malformed or conflicting snapshots raise `InvalidSnapshot`.

The candidate must equal the successful CI commit. Repository and workflow
identities must match the caller's trusted policy; only main-branch push runs
qualify. Enablement must be a boolean. Disabled, unsuccessful, or untrusted
requests cannot allocate tags or publish. There is no moving-main input.

## State transitions

| Status | Next actions |
| --- | --- |
| `disabled`, `untrusted`, `ci_incomplete` | None |
| `reserve` | Create tag at `Plan.sha` |
| `build`, `build_incomplete` | Build and verify missing assets |
| `stage` | Create draft if absent, upload missing assets |
| `publish` | Publish the fully verified draft once |
| `complete` | None |

The first stable version is `v0.1.0`; subsequent allocations increment the
largest stable version's patch numerically. Supply only canonical stable tags,
not prereleases or unrelated tag namespaces. Existing candidate tags are reused
regardless of newer tags elsewhere. Multiple stable tags at the candidate are
ambiguous and rejected. `requested_tag` identifies an existing recovery tag;
it cannot allocate an arbitrary version or retarget a tag.

Build snapshots contain verified asset SHA256 digests and the build commit.
An incomplete build cannot stage a release. A draft can publish only when its
asset inventory and digests exactly match the full build. Existing mismatched
assets and incomplete published releases fail instead of being overwritten.
A completed published release becomes a no-op when supplied its verified build
snapshot. Without that snapshot the planner requests verification/build work.

## Integration obligations

The workflow adapter remains unimplemented. It must:

- Authenticate CI provenance and use immutable workflow identity, full commit
  SHA, run ID and attempt; a caller-provided dataclass is not authentication.
- Verify candidate ancestry and release ordering before allocation. Existing
  recovery tags remain pinned even when the branch advances.
- Fetch a complete, paginated stable-tag/release inventory and peel annotated
  tags to their commit SHAs. Refuse duplicate or conflicting release objects.
- Hold a shared release lock, create tags without force, and refetch/replan after
  a collision or each completed phase. Plans do not provide atomic API writes.
- Build from `Plan.sha`, verify actual bytes and platform smoke tests, and
  obtain the expected asset inventory from trusted configuration.
- Resolve draft ownership, upload only missing assets, download and hash remote
  assets, then replan before publication. Do not execute an entire stale plan
  after a network retry without checking current state.
- Restrict publication to one privileged job and verify the published result.
  Keep publication disabled until the project readiness gates are satisfied.

Checksums here represent verified bytes supplied by the adapter; their format
alone does not prove artifact integrity. This module does not replace workflow
access controls, artifact validation, platform checks, or readiness auditing.

## Tests

From the repository root:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/release -v
```

Tests cover allocation, trust failures, strict versions, SHA conflicts, partial
builds/uploads, digest conflicts, interrupted-state recovery and completed-run
idempotency. No credentials or network access are required.

## Executable package contract

Each target archive contains sibling executables `f5y` and `f5y-toolhost`, built
with the same frozen dependencies, target, tested commit and release-version
setting. Packaging runs each executable with `--version` and requires its exact
name and release version. Helper version queries exit before directory-capability
bootstrap. The packer reads executable bytes before and after the version command
and refuses a detected change. This check assumes a trusted, stable build directory;
it is not protection against an adversary swapping files during execution.

`build.json` binds both payload hashes and versions to the commit, tag and target.
Verification checks the complete member inventory, regular executable modes,
nonempty payloads, canonical manifest bytes, checksums and exact committed notices.
The verifier does not execute archived binaries or infer their embedded versions.
It checks that their hashes match the accompanying manifest and that the manifest
claims the expected version, target and commit. An attacker controlling both the
payload and manifest can produce a self-consistent replacement, including new
archive checksums. These checks alone cannot authenticate untrusted archives or
prove actual executable versions, architecture or a compiler invocation.

Version evidence comes from executing both binaries in the trusted native build
job before packaging. Artifact provenance depends on the exact-SHA workflow and
its trusted artifact channel; the manifest is not a signature or attestation.
Native CI builds and smoke tests remain required for each supported platform.

Notice collection includes both executable packages and their target dependency
graphs. The naming scan and dependency-notice publication blocker remain in force.

## Current-state and workflow revision gates

Every release snapshot validates both the supplied historical CI attempt and the
current run. The current attempt number must still equal the supplied attempt,
and both must be completed, successful main pushes for the same candidate and
workflow. A newer attempt, including a running or failed attempt, invalidates the
older success. Recovery must await the newer successful completion event.

A requested release tag must already exist and resolve to the candidate. Missing
or retargeted tags fail; publication never converts recovery into a reservation.
Immediately before publication, the publisher refreshes CI and tag state, reads
the current release identity and all remote asset bytes, and replans with the
verified local build. Only a fresh publish plan permits PATCH; a verified complete
release returns without another publication. These reads are not an atomic GitHub
transaction, so protected tags and controlled release access remain necessary.

The workflows supply `FOUNDRY_WORKFLOW_SHA` from `github.workflow_sha`, never from
the candidate input. It must exactly equal the tested candidate before reservation,
preflight and packaging, and in each publication snapshot. The local reusable
workflow comes from the caller revision, so this also binds its definition.

This intentionally limits recovery: an older reserved candidate cannot run through
a newer default-branch workflow, even if the source checkout is pinned correctly.
An existing run retaining the matching workflow revision may retry only while its
CI attempt remains current and green. A future immutable-tag dispatch recovery
route requires separate review; there is no workflow-blob equivalence exception
or moving-branch fallback. Keep publication disabled until all readiness gates pass.
