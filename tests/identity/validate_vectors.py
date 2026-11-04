#!/usr/bin/env python3
"""Validate the neutral identity vectors against their catalog and specification.

Checks structure, error classes, the authorization check order and role matrix,
limits, state transitions, cited test evidence and row coverage in the
specification document. Uses only the standard library.
"""
import json
from pathlib import Path, PurePosixPath
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
FILE_KEYS = {'format', 'wire_names', 'section', 'cases'}
CASE_REQUIRED = {'name', 'operation', 'rows', 'implemented', 'request', 'expect'}
CASE_OPTIONAL = {'evidence', 'caller', 'given', 'limit', 'note'}
CALLER_KEYS = {'kind', 'state', 'scopes', 'workspace', 'role'}
LABELS = {'Local foundation', 'Specified', 'Held'}
GATE_ERRORS = {'unauthenticated', 'rate_limited', 'retired'}
NAME = re.compile(r'^[a-z][a-z0-9_]*$')
ROW = re.compile(r'^(?:CLI-\d{3}|API-X\d{3}|API-E[0-9A-F]{12})$')
EVIDENCE = re.compile(r'^((?:crates|apps)/[A-Za-z0-9_./-]+\.rs)::([a-z0-9_]+)$')
URL = re.compile(r'[a-z][a-z0-9+.-]*://', re.IGNORECASE)
NEUTRAL_URL = re.compile(r'^[a-z][a-z0-9+.-]*://[a-z0-9.-]+\.example(?:[/:?#]|$)')
EMAIL = re.compile(r'[^\s@]+@([^\s@]+)')
MATRIX_ROW = re.compile(r'^\| `([a-z_.]+)` \| (none|`[a-z_.]+`) \|((?: [a-z]+ \|){4})$', re.MULTILINE)


class InvalidVectors(ValueError):
    """A vector file, the catalog or the specification violates the contract."""


def read_json(path):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise InvalidVectors(f'{path}: duplicate JSON key: {key}')
            result[key] = value
        return result
    return json.loads(Path(path).read_text(encoding='utf-8'), object_pairs_hook=unique_object)


def slug(text):
    text = re.sub(r'[`*_]', '', text.strip().lower())
    return re.sub(r'[^\w\- ]', '', text).replace(' ', '-')


def sections(markdown):
    """Map heading slugs to the text up to the next heading of the same or higher level."""
    lines = markdown.splitlines()
    headings = []
    fenced = False
    for index, line in enumerate(lines):
        if line.startswith('```'):
            fenced = not fenced
        match = None if fenced else re.match(r'^(#{1,6}) (.+)$', line)
        if match:
            headings.append((index, len(match.group(1)), slug(match.group(2))))
    result = {}
    for position, (start, level, name) in enumerate(headings):
        end = next((line for line, other, _ in headings[position + 1:] if other <= level), len(lines))
        result[name] = '\n'.join(lines[start:end])
    return result


def check_catalog(catalog):
    for key in ('errors', 'causes', 'results', 'roles', 'cells', 'decisions', 'scopes',
                'credential_kinds', 'machines', 'limits', 'operations', 'rows'):
        if key not in catalog:
            raise InvalidVectors(f'catalog lacks {key}')
    if catalog.get('format') != 1 or catalog.get('wire_names') != 'provisional':
        raise InvalidVectors('catalog must be format 1 with provisional wire names')
    for name, kind in catalog['credential_kinds'].items():
        if kind.get('principal') not in ('user', 'service'):
            raise InvalidVectors(f'credential kind {name} has no principal')
        if set(kind.get('scopes', [])) - set(catalog['scopes']):
            raise InvalidVectors(f'credential kind {name} uses an unknown scope')
    for name, limit in catalog['limits'].items():
        if limit.get('kind') not in ('range', 'cap') or limit['min'] > limit['max']:
            raise InvalidVectors(f'limit {name} is malformed')
    for name, operation in catalog['operations'].items():
        if 'machine' in operation and operation['machine'] not in catalog['machines']:
            raise InvalidVectors(f'operation {name} uses an unknown machine')
        if 'roles' in operation:
            if operation['scope'] != 'none' and operation['scope'] not in catalog['scopes']:
                raise InvalidVectors(f'operation {name} uses an unknown scope')
            if set(operation['roles']) != set(catalog['roles']):
                raise InvalidVectors(f'operation {name} must give a cell for every role')
            if set(operation['roles'].values()) - set(catalog['cells']):
                raise InvalidVectors(f'operation {name} uses an unknown matrix cell')
    for row, entry in catalog['rows'].items():
        if not ROW.match(row):
            raise InvalidVectors(f'row ID is not neutral: {row}')
        if entry.get('label') not in LABELS or not entry.get('section') or not entry.get('operations'):
            raise InvalidVectors(f'row {row} lacks a label, section or operations')
        if set(entry['operations']) - set(catalog['operations']):
            raise InvalidVectors(f'row {row} names an unknown operation')
        if set(entry['held']) - set(catalog['decisions']):
            raise InvalidVectors(f'row {row} names an unknown decision')
        # A local foundation may still hold its remaining parts; a specified row holds nothing.
        if entry['label'] != 'Local foundation' and (entry['label'] == 'Held') != bool(entry['held']):
            raise InvalidVectors(f'row {row}: Held rows, and only they, name decisions')


def check_document(catalog, root):
    document = (root / catalog['document']).read_text(encoding='utf-8')
    found = sections(document)
    index = dict(re.findall(r'^\| ((?:CLI|API)-[A-Z0-9]+) \| [^|]+ \| ([A-Za-z ]+) \|', document, re.MULTILINE))
    for row, entry in catalog['rows'].items():
        text = found.get(entry['section'])
        if text is None or not re.search(rf'\b{re.escape(row)}\b', text):
            raise InvalidVectors(f'{row}: no section {entry["section"]} names it')
        if 'capabilities.json' not in text:
            raise InvalidVectors(f'{row}: section {entry["section"]} does not cite the audit path')
        if index.get(row) != entry['label']:
            raise InvalidVectors(f'{row}: coverage index label differs from {entry["label"]}')
        for decision in entry['held']:
            if not re.search(rf'\b{decision}\b', text):
                raise InvalidVectors(f'{row}: section {entry["section"]} does not cite {decision}')
    matrix = {}
    for operation, scope, cells in MATRIX_ROW.findall(found.get('role-and-permission-matrix', '')):
        matrix[operation] = (scope.strip('`'), re.findall(r'[a-z]+', cells))
    expected = {name: (operation['scope'], [operation['roles'][role] for role in catalog['roles']])
                for name, operation in catalog['operations'].items() if 'roles' in operation}
    for name in sorted(set(matrix) | set(expected)):
        if matrix.get(name) != expected.get(name):
            raise InvalidVectors(f'{name}: role matrix in the document differs from the catalog')
    return found


def check_evidence(reference, root):
    match = EVIDENCE.match(reference)
    if not match:
        raise InvalidVectors(f'evidence must be path::test_function: {reference}')
    relative, function = match.groups()
    if any(part in ('', '.', '..') for part in PurePosixPath(relative).parts):
        raise InvalidVectors(f'evidence path is unsafe: {relative}')
    path = root / relative
    if not path.is_file():
        raise InvalidVectors(f'evidence file does not exist: {relative}')
    if not re.search(rf'\bfn {function}\s*\(', path.read_text(encoding='utf-8')):
        raise InvalidVectors(f'evidence test does not exist: {reference}')


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


def check_neutral(case):
    for text in strings(case):
        if URL.search(text) and not NEUTRAL_URL.match(text):
            raise InvalidVectors(f'{case["name"]}: only reserved .example addresses are allowed')
        if text.startswith('/') and not text.startswith('/workspace/'):
            raise InvalidVectors(f'{case["name"]}: absolute paths must stay under /workspace/')
        for domain in EMAIL.findall(text):
            if not domain.endswith('.example'):
                raise InvalidVectors(f'{case["name"]}: email addresses must use .example')


def check_limit(case, catalog):
    limit = case['limit']
    definition = catalog['limits'].get(limit.get('name'))
    if definition is None or not isinstance(limit.get('value'), int):
        raise InvalidVectors(f'{case["name"]}: unknown limit or non-integer value')
    value, expect = limit['value'], case['expect']
    if value < definition['min']:
        wanted = definition['below']
    elif value > definition['max'] and definition['kind'] == 'range':
        wanted = definition['above']
    else:
        wanted = None
    if wanted and expect.get('error') != wanted:
        raise InvalidVectors(f'{case["name"]}: limit {limit["name"]}={value} must fail with {wanted}')
    if not wanted and expect.get('result') == 'error':
        raise InvalidVectors(f'{case["name"]}: limit {limit["name"]}={value} is within bounds')
    if definition['kind'] == 'cap' and value > definition['max']:
        if expect.get('effective_page') != definition['max']:
            raise InvalidVectors(f'{case["name"]}: capped limit must report {definition["max"]}')


def authorization_error(case, operation, catalog):
    """Return the class the check order requires, or None when authorization passes."""
    caller, given = case['caller'], case.get('given') or {}
    if set(caller) - CALLER_KEYS or not {'kind', 'scopes', 'workspace'} <= set(caller):
        raise InvalidVectors(f'{case["name"]}: caller needs kind, scopes and workspace only')
    kind = catalog['credential_kinds'].get(caller['kind'])
    if kind is None:
        raise InvalidVectors(f'{case["name"]}: unknown credential kind {caller["kind"]}')
    if set(caller['scopes']) - set(kind.get('scopes', catalog['scopes'])):
        raise InvalidVectors(f'{case["name"]}: caller holds scopes its kind cannot carry')
    role = caller.get('role')
    if (role is not None) != (kind['principal'] == 'user') or role not in (None, *catalog['roles']):
        raise InvalidVectors(f'{case["name"]}: only user principals carry a known role')
    state = caller.get('state', 'active')
    if state not in {'missing'} | {t[end] for t in catalog['machines']['credential'] for end in ('from', 'to')}:
        raise InvalidVectors(f'{case["name"]}: unknown credential state {state}')
    if state != 'active':
        return 'unauthenticated'
    if given.get('rate_limited'):
        return 'rate_limited'
    if operation['workspace_scoped'] and given.get('workspace', caller['workspace']) != caller['workspace']:
        return 'not_found'
    if operation['scope'] != 'none' and operation['scope'] not in caller['scopes']:
        return 'forbidden'
    cells = set(operation['roles'].values())
    if cells == {'retired'}:
        return 'retired'
    if role is None:
        cell = 'any' if cells == {'any'} else 'unknown'
    else:
        cell = operation['roles'][role]
    if cell == 'unknown':
        raise InvalidVectors(f'{case["name"]}: asserts an unknown permission for {case["operation"]}')
    return 'forbidden' if cell == 'deny' else None


def check_authorization(case, operation, catalog):
    expect = case['expect']
    if 'caller' in case:
        wanted = authorization_error(case, operation, catalog)
        if wanted and expect.get('error') != wanted:
            raise InvalidVectors(f'{case["name"]}: check order requires {wanted}')
        if wanted:
            return
    elif expect.get('error') in GATE_ERRORS:
        raise InvalidVectors(f'{case["name"]}: gate errors need a caller')
    if expect.get('error') in GATE_ERRORS or (expect.get('error') == 'forbidden' and 'cause' not in expect):
        raise InvalidVectors(f'{case["name"]}: authorization passed; forbidden needs a cause')


def check_transition(case, operation, catalog):
    transitions = catalog['machines'][operation['machine']]
    before = (case.get('given') or {}).get('state')
    after = case['expect'].get('state')
    states = {t['from'] for t in transitions} | {t['to'] for t in transitions}
    if {before, after} - states - {None}:
        raise InvalidVectors(f'{case["name"]}: unknown {operation["machine"]} state')
    if operation.get('transition'):
        triple = {'from': before, 'event': case['request'].get('event'), 'to': after}
        if triple not in transitions:
            raise InvalidVectors(f'{case["name"]}: transition is not in the catalog: {triple}')
    # Replays and rejected requests may leave the state unchanged; any change must be a transition.
    elif before and after and before != after and not any(
            t['from'] == before and t['to'] == after for t in transitions):
        raise InvalidVectors(f'{case["name"]}: {before} cannot become {after}')


def check_case(case, catalog, root):
    keys = set(case)
    if CASE_REQUIRED - keys or keys - CASE_REQUIRED - CASE_OPTIONAL:
        raise InvalidVectors(f'case has missing or unknown keys: {sorted(keys)}')
    if not NAME.match(case['name']):
        raise InvalidVectors(f'case name is not snake_case: {case["name"]}')
    operation = catalog['operations'].get(case['operation'])
    if operation is None:
        raise InvalidVectors(f'{case["name"]}: unknown operation {case["operation"]}')
    if not case['rows'] or not isinstance(case['rows'], list):
        raise InvalidVectors(f'{case["name"]}: rows must be a nonempty list')
    for row in case['rows']:
        entry = catalog['rows'].get(row)
        if entry is None or case['operation'] not in entry['operations']:
            raise InvalidVectors(f'{case["name"]}: row {row} does not cover {case["operation"]}')
    if not isinstance(case['implemented'], bool):
        raise InvalidVectors(f'{case["name"]}: implemented must be a boolean')
    evidence = case.get('evidence', [])
    if case['implemented'] != bool(evidence):
        raise InvalidVectors(f'{case["name"]}: implemented cases need evidence, and only they')
    for reference in evidence:
        check_evidence(reference, root)
    expect = case['expect']
    if not isinstance(expect, dict) or not isinstance(case['request'], dict):
        raise InvalidVectors(f'{case["name"]}: request and expect must be objects')
    if not operation.get('transition') and expect.get('result') not in catalog['results']:
        raise InvalidVectors(f'{case["name"]}: unknown or missing result')
    if ('error' in expect) != (expect.get('result') == 'error'):
        raise InvalidVectors(f'{case["name"]}: error results need exactly one error class')
    if 'error' in expect and expect['error'] not in catalog['errors']:
        raise InvalidVectors(f'{case["name"]}: unknown error class {expect["error"]}')
    if 'cause' in expect and ('error' not in expect or expect['cause'] not in catalog['causes']):
        raise InvalidVectors(f'{case["name"]}: a cause needs an error and a catalog cause code')
    if 'roles' in operation:
        check_authorization(case, operation, catalog)
    elif 'caller' in case:
        raise InvalidVectors(f'{case["name"]}: only API operations take a caller')
    if 'limit' in case:
        check_limit(case, catalog)
    if 'machine' in operation:
        check_transition(case, operation, catalog)
    check_neutral(case)


def validate(root=ROOT, directory=HERE):
    catalog = read_json(directory / 'catalog.json')
    check_catalog(catalog)
    headings = check_document(catalog, root)
    names, covered, total = set(), set(), 0
    for path in sorted((directory / 'vectors').glob('*.json')):
        document = read_json(path)
        if set(document) != FILE_KEYS or document['format'] != 1 or document['wire_names'] != 'provisional':
            raise InvalidVectors(f'{path.name}: expected format 1 with provisional wire names')
        if document['section'] not in headings:
            raise InvalidVectors(f'{path.name}: section {document["section"]} is not in the document')
        if not document['cases']:
            raise InvalidVectors(f'{path.name}: no cases')
        for case in document['cases']:
            check_case(case, catalog, root)
            if case['name'] in names:
                raise InvalidVectors(f'duplicate case name: {case["name"]}')
            names.add(case['name'])
            covered.update(case['rows'])
            total += 1
    if set(catalog['rows']) - covered:
        raise InvalidVectors(f'rows without vectors: {sorted(set(catalog["rows"]) - covered)}')
    return total


def main():
    try:
        total = validate()
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'Identity vectors invalid: {error}', file=sys.stderr)
        return 1
    print(f'Identity vectors valid: {total} cases')
    return 0


if __name__ == '__main__':
    sys.exit(main())
