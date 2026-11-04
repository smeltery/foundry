# Compatibility row registry

The registry maps neutral compatibility row IDs to the Foundry tests that
exercise them. It holds only IDs, test locations and optional opaque digests.
Titles, prose, addresses and the identities behind each row stay in the external
audit workspace; the checker rejects them here.

## Format

Each file in `rows/` is a JSON object with exactly two keys:

```json
{
  "format": 1,
  "rows": {
    "API-005": {
      "tests": [
        {
          "kind": "integration",
          "path": "apps/cli/tests/local_backend.rs",
          "function": "durable_create_list_get_and_snapshot"
        }
      ],
      "baseline": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    }
  }
}
```

- **Row ID**: one of the neutral shapes `API-nnn`, `API-Xnnn`, `AG-nn`, or
  `CLI-`, `NET-`, `CAT-`, `REL-` followed by `nnn` or `Xnnn`. Other prefixes and
  widths are rejected. A row may appear in only one file.
- **tests**: a nonempty list of citations with exactly these keys:
  - `kind`: `unit`, `integration` or `e2e`. Citations of kind `e2e` must live
    under `tests/e2e/`, and only they may.
  - `path`: a relative path under `apps/`, `crates/`, `packages/`, `scripts/`
    or `tests/` ending in `.rs`, `.py` or `.ts`.
  - `function`: a Rust `fn` name, a Python `test_` function or method name, or
    the title string passed to a TypeScript `test(...)` or `it(...)` call.
- **baseline** (optional): an opaque `sha256:` digest with 64 lowercase hex
  digits. The checker copies it into the report and does not interpret it.

Unknown keys, duplicate JSON keys, duplicate citations within a row and any
string containing an address are rejected.

## Checker

Run from the repository root:

```sh
python3 scripts/checks/compatibility.py [--report path.json]
python3 -m unittest discover -s tests/compatibility -v
```

The checker validates every file in `rows/`, then looks for each cited test in
its file: `fn name` for Rust, `def name` for Python and a `test` or `it` call
with that exact title for TypeScript. It writes a JSON evidence report to stdout
or `--report`:

```json
{
  "format": 1,
  "rows": {"API-005": {"file": "example.json", "resolved": [], "baseline": "sha256:…"}},
  "missing": [{"row": "API-005", "kind": "e2e", "path": "tests/e2e/…", "function": "…", "problem": "file does not exist"}]
}
```

It exits 1 when a file is malformed or any citation is missing. Citations under
`tests/e2e/` are accepted ahead of that tree existing; they are reported as
missing until the file and test are present.

## Limitations

The checker matches declarations by pattern and does not compile or run the
cited tests, so a listed test is not proof that it passes or that it covers the
whole row. A row's presence here is not a verified compatibility claim; the
external audit records status and evidence classes. `rows/example.json` cites
existing tests so the checker exercises real files.
