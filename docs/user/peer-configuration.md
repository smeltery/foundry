# Configure daemon peer transport

The development daemon supports explicit peer transport through its configuration
file. Start with the private directories and launch command in the
[daemon guide](daemon.md). Use the same protocol profile on every team member.

Replace the `network` field with an enabled object, for example:

```json
{
  "mode": "enabled",
  "profile": {
    "team_alpn": [102, 53, 121, 45, 116, 101, 97, 109],
    "join_alpn": [102, 53, 121, 45, 106, 111, 105, 110],
    "protocol": 1,
    "signing_prefix": "f5y/private-demo/",
    "invitation_endpoint_context": "f5y/private-demo/endpoint",
    "invitation_secret_context": "f5y/private-demo/bearer",
    "ticket_prefixes": ["f5y-demo1"]
  },
  "transport": {
    "bind_address": "127.0.0.1:0",
    "direct_paths": true,
    "relays": [],
    "relay_root_certificates": []
  },
  "seeds": [],
  "trust": {"mode": "unverified"},
  "codebook": null,
  "ticket_relay_compression": null,
  "startup": {"mode": "existing"}
}
```

This example binds loopback for experiments on one computer. Choose a reachable
bind address for another machine. A seed contains an `endpoint` public key,
`direct` socket-address array and optional `relay` HTTPS URL. Obtain public keys
and current addresses from authenticated network status or an invitation.
The daemon's identity remains in its protected state directory across restarts.

The signing prefix ends with `/`. Channel identifiers must differ and the numeric
protocol must be positive. A numeric peer-version mismatch produces a warning;
it does not replace signature, team or membership checks.

## Relays and referenced inputs

Set `relays` to the selected HTTPS relay URLs. An empty array installs no relay
fallback. Set `direct_paths` to `false` for relay-only transport. Supply DER root
certificate paths in `relay_root_certificates` for a private TLS relay. When that
array is empty, TLS uses public certificate trust. Explicit roots must all parse.

Referenced paths resolve relative to the configuration file's directory, even
when the process starts elsewhere. Public dictionaries, certificates and JWKS
files may use explicitly selected symlinks or shared files. Each read is bound to
an opened regular file and a byte limit. Resolution does not fetch remote files.
Resolved trust and dictionary bytes remain fixed for that process.

A short-code deployment supplies `codebook` with `path` and a SHA-256 `sha256`
hex digest. All participants need the exact same ordered dictionary and profile.
`ticket_relay_compression` optionally supplies a hostname suffix, including its
leading and trailing dots; normal invitation controls retain this setting.

## Startup and trust

| Startup mode | Required inputs | Behavior on an empty store |
| --- | --- | --- |
| `existing` | None | Wait unjoined for explicit form or join controls |
| `form` | `name` | Create a team; optional `relay` is probed first |
| `join_ticket` | `ticket` | Join through the invitation's authenticated peer |
| `join_code` | `code` and configured codebook | Join through the derived code endpoint |

Ticket and code startup accept optional `expected_founder`, `proof` and
`owner_only`. Code startup also accepts `expected_team`; the enabled object can
carry a common `expected_team` pin. Conflicting pins reject a join. Startup secret
inputs accept a string or an object containing a `path` field. Keep invitation and
proof files private.

An already active matching store resumes membership without redeeming its startup
invitation again. Corrupt, removed or differently bound state is not reset to an
empty team. Existing state and explicit pins still have to agree.

`trust.mode` selects `unverified` or `proof`. Proof trust supplies `issuer`,
`audience`, `token_type` and `jwks_file`. Verification uses the resolved public
keys and the actual local endpoint. There is no implicit login or identity
provider. See [identity proofs](../../crates/network/docs/proofs.md) for policy
semantics, including ownership versus current-proof validity.
