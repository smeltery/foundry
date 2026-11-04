# Provider, model and budget vectors

These independently authored vectors encode the
[provider, model and budget behavior specification](../../docs/architecture/providers.md).
Identities, keys and addresses are synthetic. Provider IDs are neutral labels
such as `fixture`, `hosted-a` and `local-a`; host names use reserved `.example`
names, IP addresses stay in loopback, private, shared or documentation ranges,
and paths stay under `/workspace/`. No third-party schema is copied. Field names
are provisional pending the naming decision.

`catalog.json` lists the neutral error classes, the effort vocabulary, endpoint
failure categories, write-only fields, the credential and login state machines,
limits and the audit rows each operation covers. Each file in `vectors/` holds
cases with an operation, rows, an optional given state, a request and the expected
result. A case marked `implemented` names the Foundry test that already proves
it; every other case is specified behavior with no implementation yet.

Run the checks from the repository root:

```sh
python3 tests/providers/validate_provider_vectors.py
python3 -m unittest discover -s tests/providers -v
```

The validator reuses the session API vector helpers. It checks structure, error
classes, limits, credential and login transitions, that write-only values never
appear in an expectation, decimal money and reservation arithmetic, recorded
`auto` resolution, per-model effort, endpoint network classes, that cited test
files and functions exist, that every audit row has vectors, and that every row
has a specification section citing the external audit file. It does not execute
Foundry, and a valid vector is not evidence that the behavior is implemented.
