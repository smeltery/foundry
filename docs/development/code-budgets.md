# Code budgets

Run `python3 scripts/checks/loc.py` from the repository root. The checker scans tracked and nonignored new source files, so new code cannot bypass the checks before staging.

## Counting policy

The budget counts physical lines, including comments and blank lines. This deliberately avoids language-dependent heuristics and discourages hiding logic in comments or compressed formatting. It applies to Rust, TypeScript, JavaScript, Python, shell, Nix, C and header files, including native test fixtures. Documentation, lockfiles and assets are not source code and are excluded by extension.

Each source file has a default limit of 400 lines. Each component also has a total limit in `scripts/checks/loc-budgets.json`. New source components must have an explicit budget. Tests count toward their configured component budget.

## Exceptions

A file exception must include its exact path, a numeric `limit`, an `owner` and a `rationale`. Keep exceptions narrow and review changes alongside the code. A component increase needs the same review as an architectural change.

Prefer cohesive modules and shared implementations. Do not delete features, meaningful tests or required notices to meet a budget. Do not minify code or split functions across arbitrary files merely to make the count pass. If a preserved implementation needs a larger baseline, document the measured scope and review it before integration.

## Restoration integration allocation

The reviewed restoration integration uses these component allocations. Counts
include tests, fixtures, comments and blank lines; the 400-line file limit remains.

| Component | Previous measured baseline | Current measured lines | Ceiling |
| --- | ---: | ---: | ---: |
| Contracts | 3,432 | 3,800 | 3,900 |
| Runtime | 9,598 | 12,741 | 13,000 |
| Service | 4,609 | 8,439 | 8,600 |

Contracts add 197 implementation lines and 171 test lines for the private live
observation exchange, reusing existing bounded payload and identity codecs.
Runtime adds 1,629 implementation lines and 1,514 test/support lines for retained
loading, observation, one-use continuation and restored capture. Restored execution
uses the existing Coordinator and publication handshake; the extracted supervision
loop is shared rather than duplicated.

Service adds 2,612 implementation lines and 1,218 test/fixture lines for loading
ownership, registered native comparison, actual grant and acceptance, fresh
admission and witnessed publication. Fresh and restored owners share publication
and persistence handlers. Their separate entry paths retain different store
credentials and cleanup obligations. The earlier cut and commit mapping helpers
are now consumed by these owners.

These allocations cover the reviewed behavior and failure tests, with narrow
correction headroom. They do not exempt tests or permit unrelated expansion. No
features, assertions or notices were removed, and no file exceptions were added.

## Current network allocation

The independently reviewed foreground network allocation is exactly 44,885
physical source lines, up from the 31,252-line baseline. The snapshot contains
265 files: 25,384 lines under `src/` (including inline tests) and 19,501 under
`tests/`. Its 125 native C fixture lines, comments and blank lines are counted.
The largest file is 389 lines; the default 400-line limit is unchanged.

The receipt-settlement correction adds 1,801 lines to the preceding allocation:
309 implementation lines and 1,492 test/support lines. Validated receipts are
registered under the current authority permit and settled before expiry or
retirement. Bounded presence snapshots preserve the last offline directory
state, while identical heartbeats avoid redundant writes after validation.
Real-driver regressions cover delayed actor delivery, unchanged five-second
expiry, authority changes, event overflow, EOF and shutdown ordering.

The increment composes explicit configuration, authenticated peer routing,
remote directory and presence, durable transport receipts and retries, team
controls, and invitation listener recovery into the foreground daemon. It reuses
the existing membership authority, protected mailbox store and persistent peer
driver. The outbox ledger correlates connection-bound attempts over durable
stored envelopes rather than introducing a second queue store.

The allocation includes actual-process direct and private TLS relay workflows,
offline and crash recovery, lost receipts, deduplication, removal, cancellation,
expiry and shutdown tests. Guard-retention and temporary SQLite journal lifecycle
regressions remain counted. Overlapping receipt-deadline fixtures were
consolidated while preserving their distinct assertions. No meaningful coverage,
features or required notices were removed or compressed to fit the allocation.

This exact ceiling includes no future growth allowance. Further CLI, client and
platform integration requires renewed measurement and review; this allocation
does not establish their completion or full compatibility.

## Earlier allocation reviews

The earlier network daemon allocation was 21,500 physical Rust lines. The transport/team
baseline was 11,582; the integrated library and local daemon measure 21,356, including all
tests, comments and blank lines. Independent review of the directory, mailbox,
request engine and foreground owner found cohesive composition without a new
service framework. The allowance also covers reviewed socket ownership and
cancelled-work lease corrections, with real-socket, process and recovery tests.
The separately reviewed code-policy binding adds 59 lines, and durable invitation
intents add 684 implementation and 637 test lines. They cover explicit migration,
atomic invitation publication, generation-bound recovery and effect acknowledgements.
The reviewed admitted-peer and JOIN increment adds 2,378 lines including tests.
It covers current membership checks, durable state and receipt exchange, bounded
JOIN transport, lossless join bootstrap and store-root/identity binding. These
library operations do not establish continuous daemon peer driving.
Each file remains below 400 lines. No feature or test was removed to fit the
allocation. The remaining 144 lines allow bounded corrections; unfinished peer
driving, team, approval and adapter integration requires another measured review before
further growth. This allocation is not evidence of full daemon completion.

The runtime component budget was 9,700 lines after reviewing private context
transport, bounded message history, capture startup, data/trace boundaries and
fresh terminal capture supervision and inert origin identity encoding.
The earlier measured baseline was 6,944 lines:
5,466 under `src/` (including unit tests) and
1,478 in integration tests and fixtures. The history limits add 47 implementation lines and 203 test lines,
including a real adapter exchange of 1,024 messages, duplicate acknowledgments,
and rejection before enqueue when identifiers, receipts or counts exceed limits.
Receipt history is retained to prevent duplicate dispatch. The 400-line file
limit remains unchanged; no features or tests were removed or compressed.
The capture handshake and ingress increments add 72 implementation lines and
181 test/fixture lines. They reuse one coordinator and process owner. This
capture startup increment adds 347 implementation/wiring lines and 465 test/fixture
lines. It keeps a single coordinator, gates delivery and tools on the owner reply,
and tests rejection, backpressure, timeout and cancellation. This allowance covers
completed startup. The reviewed data/trace increment adds 813 implementation/wiring
lines and 381 test lines. It preserves the distinct stored and wire format fields,
caps delivery history before copying content, and requires a matching persistence
acknowledgment before an instruction becomes durable. Independent review and nine
focused tests cover those boundaries. Trusted-owner attestation does not establish
native semantics.

The independently reviewed fresh-capture baseline is 8,816 lines: 4,608 of
implementation and test wiring, and 4,208 of tests and fixtures. By directory,
7,338 lines are under `src/` and 1,478 are integration tests. Cleanup and receipt
prerequisites add 576 lines to the earlier baseline; terminal supervision and
its library entrypoint add another 1,296. The latter includes 579 lines in new
terminal test modules. The remaining changes include implementation and updates
to existing fixtures; they are not all production code.

This allowance covers one shared supervisor, exact terminal binding, persisted
delivery barriers and checked cleanup before the private handoff. Independent
source review and 60 parent-run library tests cover these boundaries. The complete
100-test runtime suite and two documentation tests passed across owner runs,
including a rerun of a process-timeout fixture; the first full run was not green.
The file limit remains 400 without exceptions. Restored execution and service
publication are outside this allowance and require separate implementation review.
After removing three obsolete lint allowances, informational origin identities
add 155 implementation/wiring and 113 test lines, bringing the baseline to 9,081.

The private loading transport brings the component to 9,596 lines, a net increase
of 515 including 339 dedicated test lines. It reuses the existing child and pipe
workers, requires a scoped restoration capability, and keeps ordinary commands
and continuation unavailable. Independent source review and eight subprocess
tests cover private routing, directory/environment selection, bounded input,
saturated-output faults and owned cleanup. Earlier process regressions and strict
Clippy also pass. This allocation covers the isolated transport prerequisite;
admission transfer, durable continuation acceptance and a restored execution
controller require separate review. The 400-line file cap is unchanged.
Three parent-run tests and independent source review cover strict bounds and
historical prompt identity. These DTOs do not authorize restored execution; that
state machine remains outside this allowance.

The contracts budget was 3,550 lines for a measured 3,432-line baseline.
The prior 2,244 lines cover shared session and private context interfaces.
Capability negotiation and permission requests add 531 implementation lines,
655 test lines and two module exports. They provide pure descriptor matching
and one-use permission consumption, with 18 focused tests covering exact owner,
version and profile matching, duplicate declarations, expiry, revocation and
cross-request isolation. Existing runtime dispatch is unchanged; these helpers
do not authenticate callers or establish native execution fidelity. The budget
leaves 118 lines for corrections. All tests count and the largest file is 400
lines; no file exceptions, compressed formatting or removed tests are needed.

The CLI component budget is 4,800 lines after connecting explicit tool grants.
The reviewed 4,567-line baseline includes 1,951 Rust source lines and 2,580 Rust
test lines, plus build/fixture code. The addition includes a 78-line trusted
policy factory and real CLI/helper tests. Its per-file limit also stays at 400.

The service budget was 4,500 lines after reviewing message-admission freezing,
evidence-bearing tool receipts, explicit Ready activation control and private
capture mapping. The earlier component baseline was 3,428 lines.
The admission gate keeps
claim ownership through durable storage and queue submission and includes three
focused concurrency/lifecycle tests. The receipt increment adds 24 worker lines
and 266 test lines over the 3,083-line admission baseline. Shared helper replies
retain existing validation; the evidence response follows the fenced ledger write.
Independent review and three focused tests cover actual queued worker dispatch,
digest agreement with committed rows and rejection after an injected write failure.
These checks do not establish terminal capture or native validation. The 400-line
file limit is unchanged; later service integration needs separate review.
The Ready control adds three implementation lines and 52 test lines. Independent
review and a fourth worker test verify that capture configuration persists Ready
without opening admission, while legacy configuration retains its existing behavior.
The bounded reply-size counter and its regressions bring that baseline to 3,459.

Private snapshot and finalization mapping add 181 source lines and 500 test/fixture
lines, for a measured total of 4,140. Independent source review and four parent-run
integration tests cover exact stored/wire fields, complete snapshot evidence,
binary payload and terminal preservation, binding mismatches, and the store's
combined metadata limit. Tests obtain a real runtime cleanup handoff and publish
through the actual store API; their synthetic owner does not validate native
semantics. The mapping modules remain outside production registration until the
service owner consumes them. This allowance does not cover that future owner,
native validation or restored execution. All files remain within 400 lines.
Reviewed helper-pair reconciliation adds 86 source and 200 test lines, bringing
the baseline to 4,426. Four new regressions and two included codec tests pass;
strict scoped Clippy and independent source review also pass. The helper reuses
the existing digest codecs and separately checks receipt identity, outcome,
provenance and exact delivered bytes. It does not establish complete native
coverage, grants or trust in arbitrary caller-created objects.

The service allocation is now 4,700 lines for a measured 4,609-line component.
The 183-line net increase extracts lease thread ownership from execution and adds
three lifecycle tests. Independent source review confirms legacy stop/join and
error ordering. Tests verify real renewal while admission is frozen, both joins
after a watchdog panic, and cleanup of a partially initialized owner. The prior
61-test service run passed with one separately gated helper test ignored; the
three lifecycle tests passed separately. This allocation does not cover the
future capture owner or publication integration, and keeps the 400-line file cap.

The store budget was 9,400 lines after reviewing restore ownership, bounded
history, consistent capture snapshots, atomic continuation/message claims and
initial fresh admission. Its earlier 7,674-line baseline comprised 3,412 source
and 4,262 test lines.
Compared with atomic checkpoint finalization,
the restore increment adds 792 source
and 878 test lines for shared admission, one-use authorization, receipt validation,
history quotas and batched recovery. It retains migration, corruption, concurrency
and injected-write rollback coverage. The largest file is 289 lines; no file
exception is needed. Capture snapshots add 280 source and 466 test lines, reusing
the existing cut hashing and head validation within one fenced read transaction.
Independent review and eight focused tests cover complete coverage, bounds before
evidence copies, concurrent writes and unchanged historical hash encoding. Snapshot
ownership is historical; publication still checks the current fence and cut. Native
loading and service integration are separate work.
After prompt-decoding and record-digest prerequisites, the continuation increment
adds 259 source and 575 test lines. It shares ordinary admission and queue-claim
helpers, preserves historical generic receipt hashes and commits exact message
selection with execution ownership. Ten focused tests cover migration, metadata-only
retries, concurrent withdrawal/cancellation and full rollback, including overflow
of the second revision increment. The largest file remains 289 lines. This budget
does not cover native validation or future execution-owner integration.
Fresh admission adds 102 source and 355 test lines, for 8,131 total: 3,514 source
and 4,617 tests. It shares the existing admission transaction and prevents a new
fresh launch permit after retained execution history, including eventless attempts.
Eight parent-run tests and independent source review cover history predicates,
queue compatibility, cancellation, concurrency and rollback. The largest file
remains 289 lines. Future witness storage and continuation acceptance require
separate review and are not included in this allocation.

The independently reviewed witness codec adds 830 production and 280 test lines,
bringing the store to 9,241 physical source lines. Nine parent-run tests cover
canonical bytes and domain hashes, strict decoding, structural references and
exact count/byte limits. The codec bounds its internal allocations and rejects
duplicate fields before document decoding. It does not establish native semantic
authority. This allowance covers types and encoding only; witness publication,
association checks, quota transactions and continuation acceptance remain outside
this reviewed increment. The 400-line file limit is unchanged.

The store allocation is now 10,800 lines for the independently reviewed schema 10
witness publication implementation. Its measured total is 10,635: 5,348 under
`src/` (including unit tests) and 5,287 in integration tests and support. The
increment adds 724 source lines and 670 test lines to the reviewed codec baseline.
It reuses checkpoint finalization, request hashing, capture snapshots and quota
accounting. Review verified all 65 source/test fingerprints; 11 parent-run witness
tests and the owner's 122-test store run cover migration, corruption, evidence,
atomic rollback, concurrent publication and quota boundaries. Workspace Clippy
and dependent CLI/helper integration checks also pass.

This allocation covers bounded witness storage and transactional publication.
Native semantic validation, service capture ownership, loading snapshots and
continuation acceptance require separate implementation and budget review.
The 400-line file limit remains unchanged, with no exceptions, deleted tests or
compressed formatting. The remaining 165 lines allow small corrections.

The network allocation was 11,300 lines for the reviewed 11,139-line library:
4,589 production lines and 6,550 test/support lines. The full-team store,
transitions, migration and scrub group accounts for 2,968 physical lines;
the remaining scope includes transport, identity, membership, projection storage,
proof verification, signed records, team operations and ticket codecs. Independent
review found that the former 5,000-line ceiling predated these accepted additions.
The retained tests cover stale ownership, revision overflow, crash boundaries,
scrub uncertainty and historical membership. Largest files are 357 production
and 376 test lines. No tests are excluded from accounting. This allocation does
not cover code invitations, listener recovery, service authorization fencing,
messaging integration or further platform acceptance work.

Configured invitation-code derivation adds 150 implementation lines, 235 test
lines and one module export, bringing the network total to 11,525. Its reviewed
allocation is now 11,600. Eight tests include literal neutral vectors generated
with an independent Python BLAKE3 reference and OpenSSL Ed25519, plus normalization,
sampling, dictionary validation and input bounds. Independent source review
accepted the isolated derivation API. No deployment dictionary, hosted defaults or
listener implementation is bundled. Durable intents, policy binding, discovery
and messaging integration remain outside this increment; file limits stay at 400.

The pinned `vendor/netwatch/` dependency has an exact 5,695-line allocation,
including its tests and build script. Executable Rust code is unchanged from
version 0.19.3; the maintenance patch updates dependency constraints and one
documentation sentence. Three upstream files exceed the owned-code limit and
have path-specific exceptions at their measured sizes: 517, 1,218 and 1,159 lines.
These preserve third-party structure, with no growth allowance or test exclusions.
The dependency patch and removal criteria are recorded in
[its maintenance notes](../../vendor/netwatch/PATCHES.md). Updating this snapshot
requires renewed source and budget review. All Foundry-owned files retain the
400-line limit.

The store allocation is now 13,750 physical Rust lines for reviewed loading
snapshots and schema-11 continuation acceptance. The integrated measurement is
13,570: 6,724 under `src/` and 6,846 under `tests/`, including comments, blank
lines and test support. The 2,935-line increase over the schema-10 baseline covers
bounded shared evidence extraction, intent/acceptance integrity, atomic writes,
shared quota accounting and corruption, race, migration and retained-lineage tests.
Independent source review found no remaining correctness blocker in this bounded
store increment. The allocation leaves 180 lines for corrections; every owned
file retains the 400-line limit. No tests or behavior were removed to fit it.
Native restored execution and broader session operations are outside this increment.

The CLI allocation is now 11,950 physical lines for reviewed command parsing,
configuration and update/notice orchestration. The measured total is 11,828,
leaving 122 lines for corrections. The update/notice increment adds 233 production
and 805 fixture lines to the integrated 10,790-line baseline. It covers private
setting observations, required preparation/diagnostic/launch consumers, updated
launch handoff and notice-only suppression. Fixtures exercise the production
orchestration through parsed dispatch, including failure ordering and route
isolation. All tests remain counted and every owned file retains the 400-line
limit. This allocation does not establish full command conformance or implement
remote authentication, native services, installation or release packaging.

## Failure handling

CI provisioning has a 150-line allocation for its reviewed 128-line installer
and 13-line browser sandbox setup.
It verifies selected tool versions and reads only digest-checked executable
members from bounded utility archives. Its 72 test lines count under `tests/`;
six tests cover archive integrity, member selection and exact version checks.
Language installation uses pinned setup actions rather than custom installers.

The checker returns 1 for exceeded budgets or missing component ownership, and 2 when its inputs or Git enumeration fail. CI and local hooks must use this same command and configuration. Symlinked source files require review rather than being followed automatically.
