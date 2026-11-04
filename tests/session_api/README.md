# Session API vectors

These independently authored vectors encode the
[session API behavior specification](../../docs/architecture/session-api.md).
Identities, text and addresses are synthetic; addresses use reserved `.example`
hosts and paths stay under `/workspace/`. No third-party schema is copied. Field
names are provisional pending the naming decision.

`catalog.json` lists the neutral error classes, lifecycle states, transitions,
limits and the audit rows each operation covers. Each file in `vectors/` holds
cases with an operation, rows, an optional given state, a request and the expected
result. A case marked `implemented` names the Foundry test that already proves
it; every other case is specified behavior with no implementation yet.

Run the checks from the repository root:

```sh
python3 tests/session_api/validate_vectors.py
python3 -m unittest discover -s tests/session_api -v
```

The validator checks structure, error classes, limit boundaries, lifecycle
transitions and outcome rules, that cited test files and functions exist, that
every audit row has vectors (routed rows excepted), and that every row has a
specification section citing the external audit file. It does not execute
Foundry, and a valid vector is not evidence that the behavior is implemented.
