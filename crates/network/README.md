# Network transport foundation

This crate provides authenticated Iroh QUIC connections, explicitly configured
relay transport, typed peer frames and bounded framing. It is an internal library;
it includes typed signature verification and an in-memory membership projection.
The full-team store also persists typed team transitions and local removal; see
[durable teams](../../docs/architecture/teams.md). Network CLI commands, mailboxes,
hosted identity, service discovery and automatic connection authorization remain
unimplemented.

## Integration

Construct a `WireProfile` with both ALPNs, protocol version, signing context,
invitation contexts and ticket prefixes. There is no built-in profile.
`WireProfile::validate` checks configuration shape; it does not verify signatures
or establish compatibility with another implementation.

Pass `EndpointConfig` to `NetworkEndpoint::bind`. Supply a secret key, explicit
relay selection, bind address and direct-path choice. `RelaySelection::Disabled`
requires direct paths; `Custom` accepts distinct HTTPS relay URLs. Private relay
roots may be supplied as DER certificates. TLS verification cannot be bypassed.
No public relay or discovery service is selected implicitly.

`connect(peer, channel, timeout)` and `accept(timeout)` return Iroh connections.
The authenticated remote endpoint ID is available on the connection. Callers must
perform membership authorization before accepting application messages. Use
`online(timeout)` to await relay availability and `shutdown().await` to close the
endpoint and its owned tasks.

`read_frame` and `write_frame` accept asynchronous streams and positive operation
timeouts. Frames have a four-byte big-endian length and JSON payload, bounded to
4 MiB. EOF before a prefix returns `None`; partial prefixes, truncated payloads,
invalid JSON and oversized frames fail explicitly. Discard streams after any
codec error or cancellation because a partial frame may have been transferred.
Decoding a `PeerFrame` does not verify signed document bodies. Callers must
explicitly verify supported typed records and authorize the authenticated peer.

## Signed records and membership

`signing::{MetadataRecord, MemberRecord, SignedRecord, signing_payload}` supports
metadata and member records with real endpoint-key signatures. Signature payloads
use the explicit profile signing prefix, team, record kind and typed JSON field
order. The prefix must end in `/`; teams must be nonempty, bounded and contain no
slash or control characters. Optional metadata relay serializes as `null` when
absent. Raw JSON spelling, reordered fields and arbitrary sorted JSON are not
interchangeable signing formats. Verifying a signature alone grants no authority.

`membership::{MembershipSnapshot, MembershipState, MergeReport}` provides a
**projection**, not a complete replicated team document. Unknown snapshot fields
are rejected so invitations, link secrets and identity proofs cannot be silently
discarded. `bootstrap(expected_team, expected_founder, snapshot, profile, now_ms)`
requires independently trusted identity, verifies the founding chain and metadata,
and reports excluded entries. It does not prove the local endpoint is admitted or
that the snapshot is current.

`merge_from(authenticated_peer, snapshot, profile, now_ms)` requires an already
active peer. Supply the connection's authenticated `remote_id()`, never an ID
asserted inside JSON. The operation rejects a different team, founder or signing
domain. Valid individual changes are retained; invalid or unauthorized updates
are counted in `MergeReport.rejected`. Name conflicts needing an unavailable proof
adapter also increment `proof_required`.

Historical admissions remain first-seen. Member and metadata updates use timestamp
and signature ordering; timestamps beyond one day into the supplied future are
rejected. Historical admitted signers retain authority even after removal, while
current activity controls peer admission. This is trusted-member behavior, not
owner-only access control or adversarial key revocation. `is_active` checks current
membership; callers still own connection authorization and message dispatch.

Compatible histories and independent updates have convergence tests. Conflicting
admissions or person-name claims do not have a general convergence guarantee.
A new bootstrap from an older valid snapshot can restore stale activity; callers
must preserve trusted history across reconnects. Configuration and full team persistence remain unimplemented. The separate
[projection store](docs/persistence.md) retains supported membership history, and
local identity storage is described below.

## Full team transitions

The separate `TeamState` engine handles full typed team documents, invitations,
link rotation, admission, removal and departure in memory. Operations return new
candidates; they do not commit storage or send network replies. A caller must
commit before publishing effects and filter outgoing state for the recipient's
current membership. Inactive recipients receive neither invitations nor link secrets.

An explicitly selected proof verifier enables ownership checks and current identity
status. Without that configuration, proof-bearing input fails explicitly. Matching
single-use invitation retries preserve their local admission semantics; incoming
peer updates still undergo separate authorization checks. Independent replicas do
not provide globally exactly-once invitation consumption.

Bootstrap verifies and reports an accepted subset. It is not a lossless durable
loader for historically accepted state. Full-team persistence and service wiring
are still under development.

## Durable local identity

The separate [configured proof verifier](docs/proofs.md) validates endpoint-bound
identity tokens against explicit local public keys. It does not supply hosted
login or authorize team membership by itself.

`load_or_create_identity(&Dir, lock_timeout)` accepts an explicitly supplied private
directory capability and returns `LocalIdentity`. `endpoint_id()` returns the
public endpoint ID; `secret_key()` borrows the signing key for endpoint setup.
The wrapper has no debug or serialization implementation. Errors use fixed text
without file paths or key bytes.

The directory must already exist with effective-user ownership and Unix mode
0700. The key and advisory lock must be regular, single-link files owned by that
user with mode 0600. Opens reject final-component symlinks and use nonblocking I/O
to avoid waiting on substituted FIFOs. The capability's caller remains responsible
for how the directory was selected; the API cannot prove its original path had no
symlink components. No default home, environment lookup or migration is performed.

A positive lock timeout up to 60 seconds bounds contention. Cooperating processes
share the retained `identity.lock` file; do not delete or replace it. Kernel I/O
and scheduling have no hard deadline. Missing `machine.key` alone permits creation.
Existing malformed, unreadable or insecure files fail without repair or key
rotation. The representation is exactly 32 raw bytes, retained across reopen.

Creation writes a private temporary file, syncs it, atomically publishes with a
hard link that refuses replacement, removes the temporary name and syncs the
directory before returning success. Filesystems without the required operations
fail explicitly. A failure after publication may leave the key present; retry
loads it rather than generating another. A crash before temporary unlink can leave
a second link. Under the held lock, recovery removes only the expected public-key-
derived temporary name after verifying identical inode/device, private ownership
and permissions, and exactly two links. It then syncs the key and directory. Other
multiple links fail without rotation. Unpublished crash residue is not automatically
deleted. No disk
rollback detection, secure erasure or defense against malicious same-user writers
is claimed.

This implementation checks Unix ownership and mode bits; it does not audit extended
ACLs or provide a Windows ACL implementation. Other platforms return `Unsupported`
without storage writes. Tests on the current host do not establish platform parity.

## Validation and remaining work

Run `cargo test -p foundry-network --locked`. Local tests use ephemeral keys,
loopback addresses and a private TLS relay with an explicitly trusted certificate.
They assert direct and relay paths at both peers, verify remote identities,
exercise framing limits and failures, and check closure and relay loss. Tests
have bounded deadlines and require no public service. Additional tests cover real-key
signature vectors, exact typed field order, tampering, cross-team signatures,
unrooted admission cycles, stale snapshots, revoked signers, conflicting histories
and equal-timestamp ordering. These tests do not exercise a deployed foreign peer.

These tests establish local transport behavior. WAN traversal, cross-platform
validation, mixed-implementation compatibility, full-state adaptation, invitation
redemption/consumption, hosted proof verification, live full-state exchange and reconnection
remain separate integration requirements. These foundations do not establish full
network parity. Neither missing hosted identity nor rejected proof-dependent
updates are accepted feature omissions.
Distribution also requires the reviewed dependency notice bundle; generic license
texts are retained externally pending release clearance.
