#!/usr/bin/env python3
"""Validate the neutral session API vectors against their catalog and specification.

Checks structure, error classes, limits, lifecycle transitions, cited test evidence
and row coverage in the specification document. Uses only the standard library.
"""
import json
from pathlib import Path, PurePosixPath
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
FILE_KEYS = {'format', 'wire_names', 'section', 'cases'}
CASE_REQUIRED = {'name', 'operation', 'rows', 'implemented', 'request', 'expect'}
CASE_OPTIONAL = {'evidence', 'given', 'limit', 'note'}
NAME = re.compile(r'^[a-z][a-z0-9_]*$')
EVIDENCE = re.compile(r'^((?:crates|apps)/[A-Za-z0-9_./-]+\.rs)::([a-z0-9_]+)$')
URL = re.compile(r'[a-z][a-z0-9+.-]*://', re.IGNORECASE)
NEUTRAL_URL = re.compile(r'^[a-z][a-z0-9+.-]*://[a-z0-9.-]+\.example(?:[/:?#]|$)')
LIFECYCLE_OPERATION = 'session.lifecycle'


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
    for key in ('errors', 'results', 'lifecycles', 'outcomes', 'transitions', 'limits', 'rows'):
        if key not in catalog:
            raise InvalidVectors(f'catalog lacks {key}')
    if catalog.get('format') != 1 or catalog.get('wire_names') != 'provisional':
        raise InvalidVectors('catalog must be format 1 with provisional wire names')
    for transition in catalog['transitions']:
        if {transition['from'], transition['to']} - set(catalog['lifecycles']):
            raise InvalidVectors(f'transition uses unknown lifecycle: {transition}')
    for name, limit in catalog['limits'].items():
        if limit.get('kind') not in ('range', 'cap') or limit['min'] > limit['max']:
            raise InvalidVectors(f'limit {name} is malformed')
    for row, entry in catalog['rows'].items():
        if not re.fullmatch(r'(?:API-\d{3}|API-X\d{3}|AG-\d{2})', row):
            raise InvalidVectors(f'row ID is not neutral: {row}')
        if not entry.get('section') or not isinstance(entry.get('operations'), list):
            raise InvalidVectors(f'row {row} lacks a section or operations')
        if not entry['operations'] and not entry.get('routed'):
            raise InvalidVectors(f'row {row} has no operations and is not routed')


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


def check_lifecycle(case, catalog):
    states, outcomes = set(catalog['lifecycles']), set(catalog['outcomes'])
    given = (case.get('given') or {}).get('lifecycle')
    after = case['expect'].get('lifecycle')
    for state in (given, after):
        if state is not None and state not in states:
            raise InvalidVectors(f'{case["name"]}: unknown lifecycle {state}')
    if after == 'exit' and case['expect'].get('outcome') not in outcomes:
        raise InvalidVectors(f'{case["name"]}: exit requires an outcome')
    if after not in (None, 'exit') and case['expect'].get('outcome') is not None:
        raise InvalidVectors(f'{case["name"]}: only exit carries an outcome')
    transitions = catalog['transitions']
    if case['operation'] == LIFECYCLE_OPERATION:
        triple = {'from': given, 'event': case['request'].get('event'), 'to': after}
        if triple not in transitions:
            raise InvalidVectors(f'{case["name"]}: transition is not in the catalog: {triple}')
    elif given != after and given and after and not any(t['from'] == given and t['to'] == after for t in transitions):
        raise InvalidVectors(f'{case["name"]}: {given} cannot become {after}')


def check_case(case, catalog, root):
    keys = set(case)
    if CASE_REQUIRED - keys or keys - CASE_REQUIRED - CASE_OPTIONAL:
        raise InvalidVectors(f'case has missing or unknown keys: {sorted(keys)}')
    if not NAME.match(case['name']):
        raise InvalidVectors(f'case name is not snake_case: {case["name"]}')
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
    if case['operation'] != LIFECYCLE_OPERATION and expect.get('result') not in catalog['results']:
        raise InvalidVectors(f'{case["name"]}: unknown or missing result')
    if ('error' in expect) != (expect.get('result') == 'error'):
        raise InvalidVectors(f'{case["name"]}: error results need exactly one error class')
    if 'error' in expect and expect['error'] not in catalog['errors']:
        raise InvalidVectors(f'{case["name"]}: unknown error class {expect["error"]}')
    if 'limit' in case:
        check_limit(case, catalog)
    check_lifecycle(case, catalog)
    check_neutral(case)


def check_document(catalog, root):
    document = (root / catalog['document']).read_text(encoding='utf-8')
    found = sections(document)
    index = set(re.findall(r'^\| ((?:API|AG)-[A-Z0-9]+) \|', document, re.MULTILINE))
    for row, entry in catalog['rows'].items():
        text = found.get(entry['section'])
        if text is None or not re.search(rf'\b{re.escape(row)}\b', text):
            raise InvalidVectors(f'{row}: no section {entry["section"]} names it')
        if 'capabilities.json' not in text:
            raise InvalidVectors(f'{row}: section {entry["section"]} does not cite the audit path')
        if row not in index:
            raise InvalidVectors(f'{row}: missing from the coverage index')
    return found


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
    required = {row for row, entry in catalog['rows'].items() if not entry.get('routed')}
    if required - covered:
        raise InvalidVectors(f'rows without vectors: {sorted(required - covered)}')
    return total


def main():
    try:
        total = validate()
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'Session API vectors invalid: {error}', file=sys.stderr)
        return 1
    print(f'Session API vectors valid: {total} cases')
    return 0


if __name__ == '__main__':
    sys.exit(main())
