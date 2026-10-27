# Invitation tickets

`ticket::TicketCodec` encodes and decodes invitation payloads without contacting
peers or granting membership. The caller supplies a validated `WireProfile`.
Its first ticket prefix is emitted; all listed prefixes are accepted on input.
Prefixes must not overlap: one cannot be the beginning of another accepted prefix.
There is no default prefix, discovery service or hosted join page.

A ticket contains a team identifier, bearer secret, display name, optional team
relay, first-peer relay/IP hints and endpoint public keys. `Ticket` deliberately
has no `Debug` implementation. Send encoded invitations only to intended
recipients; do not include them in ordinary logs or diagnostics.

## Encoding and decoding

The binary payload places the 16-byte team ID and 16-byte secret first, followed
by three length-prefixed text fields, address hints and 32-byte endpoint keys.
It is encoded as unpadded URL-safe Base64 after the configured prefix. Decoding
also accepts the final fragment of a URL. A complete public-key tail with at
least one peer is required.

Encoding retains the first 60 name characters and truncates each text field to
250 UTF-8 bytes without splitting a character. It emits at most two IP hints.
Decoding reads all hints indicated by the encoded byte count and uses lossy
UTF-8 conversion for text. Consequently, decoding and re-encoding arbitrary input
can normalize text or reduce its address hints; it is not a byte-preserving relay.

Compressed relay regions require an explicit `RelayCompression` suffix. An
unconfigured compression marker is rejected. The marker is accepted in each of
the three text fields, while encoding uses compression only for relay fields.
This interpretation is part of the wire format, not validation of a relay host.

Payloads above 65,536 decoded bytes are rejected. Encoded length is checked before
Base64 allocation; peer counts are checked before encoding. Invalid hex input,
truncated fields, malformed Base64 and incomplete peer keys return fixed errors
without echoing the invitation. These bounds do not constrain the caller's
already allocated input or establish a whole-process memory limit.

## Address hints and integration

`TicketCodec::redact` replaces whitespace-delimited words containing a configured
prefix followed by more than 21 Base64 characters. It catches partial payloads
that already expose secret bits, without requiring a valid decodable ticket.
Invisible separators and Markdown escapes cannot hide those fragments. Adjacent
ASCII letters suppress a prefix match; digits, punctuation and non-ASCII letters
do not. Whitespace is preserved. This is configured ticket detection, not general
credential removal, and callers must apply it before sharing display text.

`display_name` replaces control characters and Unicode line/paragraph separators
with spaces, then limits display to 60 characters including an ellipsis when
truncated. It does not modify the signed team name or redact ticket contents.

`Ticket::peer_addresses` applies a team relay to every peer. Without a team relay,
only the first peer receives the relay hint; IP hints also apply only to that
peer. An invalid relay URL is ignored when constructing addresses. Connection,
membership verification and invitation redemption remain separate operations.

This codec does not implement four-word codes, listener persistence, a join
service or CLI commands. Tests cover neutral
golden bytes, prefix and URL handling, truncation, Unicode, compressed fields and
address selection. They do not establish interoperability with a deployed peer.
