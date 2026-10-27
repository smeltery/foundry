# Naming policy checks

The checker reads a trusted external policy. No policy or forbidden vocabulary is
embedded in this repository. It never downloads policy or external Git objects.

```sh
python3 scripts/checks/naming.py --root . --policy-json-env FOUNDRY_NAMING_POLICY
python3 scripts/checks/naming.py --root . --policy /external/policy.json
python3 scripts/checks/naming.py --root . --mode history --policy /external/policy.json
python3 scripts/checks/naming.py --mode release --release dist --policy /external/policy.json
python3 -m unittest discover -s tests/naming -v
```

`--policy` takes precedence over environment input. The default environment key is
`F5Y_NAMING_POLICY_JSON`; CI should explicitly select `FOUNDRY_NAMING_POLICY` as
shown above. Policy files and JSON must remain outside the repository.

The policy schema is an object containing `version: 1` and a nonempty `rules`
array. Each rule contains exactly `id` and `pattern`. IDs must be `N` followed by
three to six digits. Patterns use Python regular expressions, always ignoring
case. Duplicate IDs, malformed regexes, and patterns matching the empty string
are rejected. Patterns are trusted configuration, not an untrusted regex sandbox.

The default `tracked` mode scans **index blobs**, including staged changes. It
does not scan unstaged edits or untracked files. `history` scans commits reachable
from local refs, their file trees, commit records, and annotated tag records.
Deleted historical files are included; identical content with the same suffix is
inspected once. Reflogs, unreachable objects, external submodule repositories,
remote refs not present locally, and external large-file objects are not fetched.
Shallow history fails explicitly. Git replacement objects are disabled.

Release mode accepts a directory, file, ZIP, TAR, or compressed TAR. Nested
archives are bounded. Archives are inspected in memory without extraction;
absolute/traversal paths and links are reported as uninspected. Filesystem links
are not followed. Git symlink target text is scanned, but the target is not
followed. Size, total-byte, entry, commit, and nesting limits all fail closed.
Compressed TAR expansion includes headers and padding in its separate total
expansion budget. Limits can be set with `--max-bytes`, `--max-total-bytes`,
`--max-files`, `--max-commits`, and `--max-depth`.

Output is JSON with neutral rule IDs, redacted paths, occurrence counts, and the
first matching text line per rule/object. No matching text or regex is printed.
`uninspected` lists binary/undecodable files, visual assets, external objects,
links, corrupt archives, and other coverage gaps. SVG text is scanned, but no
image recognition or OCR is performed. A separate asset inspection process is
required; this checker provides no implicit visual clearance.

Exit codes:

- `0`: all selected content inspected, no matches.
- `1`: at least one match; inspect `errors` and `uninspected` for additional gaps.
- `2`: invalid policy/input, exceeded bounds, or incomplete inspection.

The tests use neutral synthetic terms and temporary local Git repositories.
They require Python and Git, but no network, package installation, or credentials.
