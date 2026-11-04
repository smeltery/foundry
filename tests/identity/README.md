# Identity vectors

These independently authored vectors encode the
[account, workspace and authorization specification](../../docs/architecture/identity.md).
Identities, names and addresses are synthetic; email addresses and hosts use
reserved `.example` domains, and credential values are placeholders. No
third-party schema is copied. Operation, scope, field and cause names are
provisional pending the naming and transport decisions.

`catalog.json` lists the neutral error classes and causes, roles, scopes,
credential kinds, the role and permission matrix, state machines, limits and the
audit rows each operation covers. Each file in `vectors/` holds cases with an
operation, rows, an optional caller, an optional given state, a request and the
expected result. A case marked `implemented` names the Foundry test that already
proves it; every other case is specified behavior with no implementation yet.

Run the checks from the repository root:

```sh
python3 tests/identity/validate_vectors.py
python3 -m unittest discover -s tests/identity -v
```

The validator checks structure, error classes, the authorization check order
against the matrix (and rejects any case that asserts an `unknown` cell), limit
boundaries, state transitions, cited test files and functions, that every audit
row has vectors, and that every row has a specification section that cites the
external audit file, its coverage label and any held decision. It does not
execute Foundry, and a valid vector is not evidence that the behavior is
implemented.
