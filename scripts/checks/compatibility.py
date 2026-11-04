#!/usr/bin/env python3
"""Check the neutral compatibility row registry and emit a JSON evidence report.

Each registry file maps neutral row IDs to the Foundry tests that cite them. The
checker validates ID syntax, the closed registry shape, that no string carries an
address or prose, and that every cited test function exists in the cited file.
Uses only the standard library and never executes the cited tests.
"""
import argparse
import json
from pathlib import Path, PurePosixPath
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
ROWS = ROOT / 'tests/compatibility/rows'
FORMAT = 1
FILE_KEYS = {'format', 'rows'}
ROW_KEYS = {'tests'}
ROW_OPTIONAL = {'baseline'}
CITATION_KEYS = {'kind', 'path', 'function'}
KINDS = {'unit', 'integration', 'e2e'}
# Neutral row ID shapes shared with the external ledger; the ledger itself stays external.
ROW_ID = re.compile(r'(?:API-(?:\d{3}|X\d{3})|AG-\d{2}|(?:CLI|NET|CAT|REL)-X?\d{3})')
BASELINE = re.compile(r'sha256:[0-9a-f]{64}')
PATH_ROOTS = ('apps/', 'crates/', 'packages/', 'scripts/', 'tests/')
E2E_ROOT = 'tests/e2e/'
PATH_PART = re.compile(r'[A-Za-z0-9_][A-Za-z0-9_.-]*')
IDENTIFIER = re.compile(r'[a-z_][a-z0-9_]*')
PYTHON_TEST = re.compile(r'test_[a-z0-9_]+')
TS_TEST = re.compile(r'[A-Za-z0-9][A-Za-z0-9 ,.:;()/+=<>-]{0,158}[A-Za-z0-9)]')
URL = re.compile(r'[a-z][a-z0-9+.-]*://|www\.', re.IGNORECASE)


class InvalidRegistry(ValueError):
    """A registry file violates the neutral row registry contract."""


def read_json(path):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise InvalidRegistry(f'{path.name}: duplicate JSON key: {key}')
            result[key] = value
        return result
    return json.loads(path.read_text(encoding='utf-8'), object_pairs_hook=unique_object)


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for key, item in value.items():
            yield key
            yield from strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)


def check_keys(value, required, optional, where):
    if not isinstance(value, dict):
        raise InvalidRegistry(f'{where}: expected an object')
    keys = set(value)
    if required - keys or keys - required - optional:
        raise InvalidRegistry(f'{where}: missing or unknown keys: {sorted(keys)}')


def check_path(relative, kind, where):
    if not isinstance(relative, str) or not relative.startswith(PATH_ROOTS):
        raise InvalidRegistry(f'{where}: test path must be relative under {PATH_ROOTS}')
    parts = PurePosixPath(relative).parts
    if any(not PATH_PART.fullmatch(part) for part in parts) or '/'.join(parts) != relative:
        raise InvalidRegistry(f'{where}: test path is unsafe: {relative}')
    if PurePosixPath(relative).suffix not in ('.rs', '.py', '.ts'):
        raise InvalidRegistry(f'{where}: test path must be a .rs, .py or .ts file: {relative}')
    if (kind == 'e2e') != relative.startswith(E2E_ROOT):
        raise InvalidRegistry(f'{where}: e2e citations, and only they, live under {E2E_ROOT}')


def check_function(suffix, function, where):
    pattern = {'.rs': IDENTIFIER, '.py': PYTHON_TEST, '.ts': TS_TEST}[suffix]
    if not isinstance(function, str) or not pattern.fullmatch(function):
        raise InvalidRegistry(f'{where}: test function is not a {suffix} test name: {function!r}')


def check_citation(citation, where):
    check_keys(citation, CITATION_KEYS, set(), where)
    if citation['kind'] not in KINDS:
        raise InvalidRegistry(f'{where}: kind must be one of {sorted(KINDS)}')
    check_path(citation['path'], citation['kind'], where)
    check_function(PurePosixPath(citation['path']).suffix, citation['function'], where)


def check_row(row, entry, where):
    if not isinstance(row, str) or not ROW_ID.fullmatch(row):
        raise InvalidRegistry(f'{where}: row ID is not a neutral ID: {row!r}')
    check_keys(entry, ROW_KEYS, ROW_OPTIONAL, f'{where}: {row}')
    if 'baseline' in entry and not (isinstance(entry['baseline'], str)
                                    and BASELINE.fullmatch(entry['baseline'])):
        raise InvalidRegistry(f'{where}: {row}: baseline must be sha256:<64 lowercase hex>')
    tests = entry['tests']
    if not isinstance(tests, list) or not tests:
        raise InvalidRegistry(f'{where}: {row}: tests must be a nonempty list')
    seen = set()
    for index, citation in enumerate(tests):
        check_citation(citation, f'{where}: {row}.tests[{index}]')
        identity = (citation['path'], citation['function'])
        if identity in seen:
            raise InvalidRegistry(f'{where}: {row}: duplicate citation {"::".join(identity)}')
        seen.add(identity)


def check_document(document, where):
    check_keys(document, FILE_KEYS, set(), where)
    if document['format'] != FORMAT:
        raise InvalidRegistry(f'{where}: format must be {FORMAT}')
    if not isinstance(document['rows'], dict) or not document['rows']:
        raise InvalidRegistry(f'{where}: rows must be a nonempty object')
    for text in strings(document):
        if URL.search(text):
            raise InvalidRegistry(f'{where}: addresses are not allowed: {text!r}')
    for row, entry in document['rows'].items():
        check_row(row, entry, where)


def defines(source, suffix, function):
    """Report whether the source text declares the named test."""
    name = re.escape(function)
    if suffix == '.rs':
        pattern = rf'\bfn {name}\s*[<(]'
    elif suffix == '.py':
        pattern = rf'^\s*(?:async\s+)?def {name}\s*\('
    else:
        pattern = rf'\b(?:test|it)(?:\.\w+)*\(\s*(["\'`]){name}\1'
    return re.search(pattern, source, re.MULTILINE) is not None


def resolve(citation, root):
    """Return None when the cited test exists, or a short description of the gap."""
    path = root / citation['path']
    if path.is_symlink() or not path.is_file():
        return 'file does not exist'
    if not path.resolve().is_relative_to(root.resolve()):
        return 'file resolves outside the repository'
    if not defines(path.read_text(encoding='utf-8'), path.suffix, citation['function']):
        return 'test function does not exist'
    return None


def build_report(root=ROOT, directory=ROWS):
    files = sorted(directory.glob('*.json'))
    if not files:
        raise InvalidRegistry(f'no registry files in {directory}')
    rows, missing = {}, []
    for path in files:
        document = read_json(path)
        check_document(document, path.name)
        for row, entry in document['rows'].items():
            if row in rows:
                raise InvalidRegistry(f'{path.name}: row {row} is registered more than once')
            resolved = []
            for citation in entry['tests']:
                problem = resolve(citation, root)
                if problem is None:
                    resolved.append(citation)
                else:
                    missing.append({'row': row, **citation, 'problem': problem})
            rows[row] = {'file': path.name, 'resolved': resolved}
            if 'baseline' in entry:
                rows[row]['baseline'] = entry['baseline']
    return {'format': FORMAT, 'rows': dict(sorted(rows.items())), 'missing': missing}


def main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--rows', type=Path, help='registry directory (default: tests/compatibility/rows)')
    parser.add_argument('--report', type=Path, help='write the JSON report here instead of stdout')
    args = parser.parse_args(arguments)
    root = args.root.resolve()
    try:
        report = build_report(root, args.rows or root / 'tests/compatibility/rows')
        text = json.dumps(report, indent=2) + '\n'
        if args.report:
            args.report.write_text(text, encoding='utf-8')
        else:
            sys.stdout.write(text)
    except (OSError, ValueError) as error:
        print(f'Compatibility registry invalid: {error}', file=sys.stderr)
        return 1
    for gap in report['missing']:
        print(f'{gap["row"]}: {gap["path"]}::{gap["function"]}: {gap["problem"]}', file=sys.stderr)
    cited = sum(len(entry['resolved']) for entry in report['rows'].values()) + len(report['missing'])
    print(f'Compatibility registry: {len(report["rows"])} rows, {cited} citations, '
          f'{len(report["missing"])} missing', file=sys.stderr)
    return 1 if report['missing'] else 0


if __name__ == '__main__':
    sys.exit(main())
