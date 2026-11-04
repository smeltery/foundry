#!/usr/bin/env python3
"""Validate the neutral provider, model and budget vectors against their catalog.

Reuses the session API vector helpers for JSON decoding, headings and cited test
evidence, then checks provider-specific rules: credential and login transitions,
write-only values, decimal money and reservations, recorded auto resolution,
per-model effort and endpoint network classes. Uses only the standard library.
"""
from decimal import Decimal
import ipaddress
from pathlib import Path
import re
import sys
from urllib.parse import urlsplit

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE.parent / 'session_api'))
from validate_vectors import (  # noqa: E402
    InvalidVectors, check_evidence, read_json, sections, strings,
)

FILE_KEYS = {'format', 'wire_names', 'section', 'cases'}
CASE_REQUIRED = {'name', 'operation', 'rows', 'implemented', 'request', 'expect'}
CASE_OPTIONAL = {'evidence', 'given', 'limit', 'note'}
NAME = re.compile(r'^[a-z][a-z0-9_]*$')
ROW = re.compile(r'^(?:CLI-\d{3}|AG-\d{2}|API-E[0-9A-F]{12})$')
PROVIDER_ID = re.compile(r'^[a-z][a-z0-9-]{0,63}$')
MONEY = re.compile(r'^\d+(?:\.\d{1,9})?$')
MONEY_KEYS = {'ceiling', 'committed', 'reserved', 'amount', 'actual', 'released', 'spend'}
URL = re.compile(r'[a-z][a-z0-9+.-]*://', re.IGNORECASE)
MACHINES = {'credential': ('credentials.', 'credential.'), 'oauth_login': ('oauth_logins.', 'oauth_login.')}
NETWORKS = {
    'loopback': ('127.0.0.0/8', '::1/128'),
    'lan': ('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', 'fc00::/7'),
    'tailnet': ('100.64.0.0/10',),
    'link_local': ('169.254.0.0/16', 'fe80::/10'),
}


def network_class(address):
    ip = ipaddress.ip_address(address)
    for name, blocks in NETWORKS.items():
        if any(ip in ipaddress.ip_network(block) for block in blocks):
            return name
    return 'public'


def check_catalog(catalog):
    for key in ('errors', 'results', 'efforts', 'endpoint_failures', 'write_only_fields',
                'machines', 'limits', 'rows'):
        if key not in catalog:
            raise InvalidVectors(f'catalog lacks {key}')
    if catalog.get('format') != 1 or catalog.get('wire_names') != 'provisional':
        raise InvalidVectors('catalog must be format 1 with provisional wire names')
    for name, machine in catalog['machines'].items():
        for transition in machine['transitions']:
            if {transition['from'], transition['to']} - set(machine['states']):
                raise InvalidVectors(f'{name} transition uses unknown state: {transition}')
    for name, limit in catalog['limits'].items():
        if limit.get('kind') != 'range' or limit['min'] > limit['max']:
            raise InvalidVectors(f'limit {name} is malformed')
    for row, entry in catalog['rows'].items():
        if not ROW.match(row):
            raise InvalidVectors(f'row ID is not neutral: {row}')
        if not entry.get('section') or not entry.get('operations'):
            raise InvalidVectors(f'row {row} lacks a section or operations')


def check_neutral(case):
    for text in strings(case):
        if URL.search(text):
            host = urlsplit(text).hostname or ''
            try:
                reserved = not ipaddress.ip_address(host).is_global
            except ValueError:
                reserved = host.endswith('.example')
            if not reserved:
                raise InvalidVectors(f'{case["name"]}: only .example or non-global addresses are allowed')
        if text.startswith('/') and not text.startswith('/workspace/'):
            raise InvalidVectors(f'{case["name"]}: absolute paths must stay under /workspace/')
        try:
            address = ipaddress.ip_address(text)
        except ValueError:
            continue
        if address.is_global:
            raise InvalidVectors(f'{case["name"]}: global address {text}')


def values_of(value, keys):
    """Yield every value stored under one of the keys, at any depth."""
    if isinstance(value, dict):
        for key, item in value.items():
            if key in keys:
                yield key, item
            yield from values_of(item, keys)
    elif isinstance(value, list):
        for item in value:
            yield from values_of(item, keys)


def check_write_only(case, catalog):
    hidden = set(catalog['write_only_fields'])
    if case['operation'] == 'endpoint.failure':
        hidden.add('base_url')
    secrets = [value for _, value in values_of(case['request'], hidden) if isinstance(value, str) and value]
    for text in strings(case['expect']):
        if any(secret in text for secret in secrets):
            raise InvalidVectors(f'{case["name"]}: expectation echoes a write-only value')


def check_limit(case, catalog):
    limit = case['limit']
    definition = catalog['limits'].get(limit.get('name'))
    if definition is None or not isinstance(limit.get('value'), int):
        raise InvalidVectors(f'{case["name"]}: unknown limit or non-integer value')
    value, expect = limit['value'], case['expect']
    wanted = None
    if value == 0 and definition.get('zero_allowed'):
        wanted = None
    elif value < definition['min']:
        wanted = definition['below']
    elif value > definition['max']:
        wanted = definition['above']
    if wanted and expect.get('error') != wanted:
        raise InvalidVectors(f'{case["name"]}: limit {limit["name"]}={value} must fail with {wanted}')
    if not wanted and expect.get('result') == 'error':
        raise InvalidVectors(f'{case["name"]}: limit {limit["name"]}={value} is within bounds')


def check_machine(case, catalog):
    operation = case['operation']
    for name, prefixes in MACHINES.items():
        if not operation.startswith(prefixes):
            continue
        machine = catalog['machines'][name]
        given = (case.get('given') or {}).get('state')
        after = case['expect'].get('state')
        for state in (given, after):
            if state is not None and state not in machine['states']:
                raise InvalidVectors(f'{case["name"]}: unknown {name} state {state}')
        if operation == prefixes[1] + 'state':
            triple = {'from': given, 'event': case['request'].get('event'), 'to': after}
            if triple not in machine['transitions']:
                raise InvalidVectors(f'{case["name"]}: transition is not in the catalog: {triple}')
        elif given and after and given != after and not any(
                t['from'] == given and t['to'] == after for t in machine['transitions']):
            raise InvalidVectors(f'{case["name"]}: {name} {given} cannot become {after}')


def money(case, value):
    if not isinstance(value, str) or not MONEY.match(value):
        raise InvalidVectors(f'{case["name"]}: money must be a decimal string with at most 9 fractional digits')
    return Decimal(value)


def check_budget(case):
    operation, given, request, expect = (case['operation'], case.get('given') or {},
                                         case['request'], case['expect'])
    if expect.get('error') != 'invalid_request':
        for _, value in values_of(case, MONEY_KEYS):
            money(case, value)
    if operation == 'budget.reserve' and 'ceiling' in given:
        room = money(case, given['ceiling']) - money(case, given['committed']) - money(case, given['reserved'])
        amounts = [money(case, amount) for amount in request['amounts']]
        if len(set(amounts)) != 1:
            raise InvalidVectors(f'{case["name"]}: concurrent reservations must use equal amounts')
        winners = min(len(amounts), int(room // amounts[0])) if room >= 0 else 0
        if expect.get('winners') != winners:
            raise InvalidVectors(f'{case["name"]}: {winners} of {len(amounts)} reservations fit')
        wanted = 'budget_exhausted' if winners < len(amounts) else None
        if expect.get('error') != wanted:
            raise InvalidVectors(f'{case["name"]}: refused reservations fail with budget_exhausted')
    if operation == 'budget.settle':
        reserved, actual = money(case, given['reservation']), money(case, request['actual'])
        committed = money(case, given['committed']) + actual
        if (money(case, expect['committed']) != committed
                or money(case, expect['released']) != max(Decimal(0), reserved - actual)
                or expect['overrun'] != (actual > reserved)
                or expect['dispatch_allowed'] == (actual > reserved)):
            raise InvalidVectors(f'{case["name"]}: settlement arithmetic is wrong')
    if operation in ('budget.deadline', 'budget.review') and expect.get('outcome') == 'success':
        raise InvalidVectors(f'{case["name"]}: a deadline or unverified review never reports success')


def check_model_selection(case, catalog):
    given, request, expect = case.get('given') or {}, case['request'], case['expect']
    models = {model['id']: model for model in given.get('models', [])}
    for model in models.values():
        if set(model.get('efforts', [])) - set(catalog['efforts']):
            raise InvalidVectors(f'{case["name"]}: model {model["id"]} lists an effort outside the vocabulary')
    if case['operation'] != 'session.model_selection' or (expect['result'] == 'error' and not models):
        return
    requested = request.get('model', 'auto')
    chosen = requested
    if requested == 'auto' and expect['result'] != 'error':
        resolution = expect.get('resolution') or {}
        keys = {'requested', 'resolved_model', 'provider_id', 'catalog_revision', 'rule'}
        if (keys - set(resolution) or resolution['requested'] != 'auto'
                or resolution['resolved_model'] in ('', 'auto')
                or expect.get('model') != resolution['resolved_model'] or expect.get('recorded') is not True):
            raise InvalidVectors(f'{case["name"]}: auto must resolve to a recorded concrete model')
        chosen = resolution['resolved_model']
    entry = models.get(chosen)
    if expect['result'] != 'error' and models and (entry is None or not entry.get('available')):
        raise InvalidVectors(f'{case["name"]}: resolved model {chosen} is not listed and available')
    effort = request.get('effort')
    if effort and entry is not None and effort not in entry.get('efforts', []):
        if expect.get('error') != 'invalid_request':
            raise InvalidVectors(f'{case["name"]}: effort {effort} is not offered by {chosen}')


def expected_admission(request, given):
    parts = urlsplit(request['base_url'])
    if request.get('models') == [] or parts.scheme not in ('http', 'https') or parts.username or parts.fragment or not parts.hostname:
        return 'invalid_request', None
    try:
        addresses = [str(ipaddress.ip_address(parts.hostname))]
    except ValueError:
        addresses = given.get('resolved') or []
    classes = {network_class(address) for address in addresses}
    if len(classes) != 1:
        return 'endpoint_not_permitted', None
    name = classes.pop()
    if name == 'link_local':
        return 'endpoint_not_permitted', None
    if name == 'public':
        return (None if parts.scheme == 'https' else 'insecure_transport'), name
    return (None if name in request.get('opt_in', []) else 'endpoint_not_permitted'), name


def check_endpoint(case, catalog):
    expect = case['expect']
    if case['operation'] == 'endpoint.failure':
        if expect.get('error') != 'endpoint_failure' or expect.get('category') not in catalog['endpoint_failures']:
            raise InvalidVectors(f'{case["name"]}: endpoint failures need a known category')
        return
    error, name = expected_admission(case['request'], case.get('given') or {})
    if expect.get('error') != error or (error is None and expect.get('network_class') != name):
        raise InvalidVectors(f'{case["name"]}: endpoint admission must be {error or name}')


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
    is_transition = case['operation'].endswith('.state')
    if not is_transition and expect.get('result') not in catalog['results']:
        raise InvalidVectors(f'{case["name"]}: unknown or missing result')
    if ('error' in expect) != (expect.get('result') == 'error'):
        raise InvalidVectors(f'{case["name"]}: error results need exactly one error class')
    if 'error' in expect and expect['error'] not in catalog['errors']:
        raise InvalidVectors(f'{case["name"]}: unknown error class {expect["error"]}')
    for _, provider in values_of(case['request'], {'provider_id'}):
        if not isinstance(provider, str) or not PROVIDER_ID.match(provider) and expect.get('error') != 'invalid_request':
            raise InvalidVectors(f'{case["name"]}: provider ID is not neutral: {provider}')
    if 'limit' in case:
        check_limit(case, catalog)
    check_machine(case, catalog)
    if case['operation'].startswith(('budget.', 'consumption.', 'session.rollover')):
        check_budget(case)
    if case['operation'] in ('session.model_selection', 'models.list'):
        check_model_selection(case, catalog)
    if case['operation'].startswith('endpoint.'):
        check_endpoint(case, catalog)
    check_write_only(case, catalog)
    check_neutral(case)


def check_document(catalog, root):
    document = (root / catalog['document']).read_text(encoding='utf-8')
    found = sections(document)
    index = set(re.findall(r'^\| ((?:API|AG|CLI)-[A-Z0-9]+) \|', document, re.MULTILINE))
    for row, entry in catalog['rows'].items():
        text = found.get(entry['section'])
        if text is None or not re.search(rf'\b{re.escape(row)}\b', text):
            raise InvalidVectors(f'{row}: no section {entry["section"]} names it')
        if 'capabilities.json' not in text:
            raise InvalidVectors(f'{row}: section {entry["section"]} does not cite the audit path')
        if row not in index:
            raise InvalidVectors(f'{row}: missing from the coverage index')
    if index - set(catalog['rows']):
        raise InvalidVectors(f'coverage index rows missing from the catalog: {sorted(index - set(catalog["rows"]))}')
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
    if set(catalog['rows']) - covered:
        raise InvalidVectors(f'rows without vectors: {sorted(set(catalog["rows"]) - covered)}')
    return total


def main():
    try:
        total = validate()
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'Provider vectors invalid: {error}', file=sys.stderr)
        return 1
    print(f'Provider vectors valid: {total} cases')
    return 0


if __name__ == '__main__':
    sys.exit(main())
