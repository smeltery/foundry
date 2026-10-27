#!/usr/bin/env python3
"""Validate catalog documents using the authored schema and cross-field checks.

This deliberately supports only the schema keywords used by this repository;
unsupported keywords fail closed. It is not a general JSON Schema implementation.
"""
import argparse
import json
from pathlib import Path, PurePosixPath
import re
import sys
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = ROOT / 'marketplace/schema.json'
KEYWORDS = {
    '$schema', '$id', '$defs', '$ref', 'title', 'description', 'type', 'properties',
    'required', 'additionalProperties', 'items', 'minItems', 'minLength',
    'pattern', 'format', 'const', 'enum', 'oneOf', 'propertyNames',
}


class InvalidCatalog(ValueError):
    """A catalog or its schema violates the supported contract."""


def read_json(path):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise InvalidCatalog(f'duplicate JSON key: {key}')
            result[key] = value
        return result
    return json.loads(Path(path).read_text(), object_pairs_hook=unique_object)


def safe_path(value):
    return (bool(value) and not value.startswith('/')
            and not re.search(r'[\\:%\x00-\x1f\x7f]', value)
            and all(part not in ('', '.', '..') for part in value.split('/'))
            and not PurePosixPath(value).is_absolute())


def safe_url(value):
    try:
        parsed = urlsplit(value)
        _ = parsed.port
        return (parsed.scheme in ('http', 'https') and bool(parsed.hostname)
                and not parsed.username and not parsed.password
                and not re.search(r'[\s\\\x00-\x1f\x7f]', value))
    except ValueError:
        return False


def check_schema(schema):
    unknown = set(schema) - KEYWORDS
    if unknown:
        raise InvalidCatalog(f'unsupported schema keywords: {sorted(unknown)}')
    for mapping in ('$defs', 'properties'):
        for child in schema.get(mapping, {}).values():
            check_schema(child)
    for child in schema.get('oneOf', []):
        check_schema(child)
    for key in ('items', 'propertyNames', 'additionalProperties'):
        if isinstance(schema.get(key), dict):
            check_schema(schema[key])
    if schema.get('format') not in (None, 'uri', 'relative-path'):
        raise InvalidCatalog(f"unsupported format: {schema['format']}")


def validate_node(value, schema, root, location='$'):
    def require(condition, message):
        if not condition:
            raise InvalidCatalog(f'{location}: {message}')
    if '$ref' in schema:
        reference = schema['$ref']
        require(reference.startswith('#/$defs/'), 'unsupported schema reference')
        name = reference.removeprefix('#/$defs/')
        require(name in root['$defs'], 'unknown schema reference')
        validate_node(value, root['$defs'][name], root, location)
    if 'oneOf' in schema:
        matches = 0
        for candidate in schema['oneOf']:
            try:
                validate_node(value, candidate, root, location)
                matches += 1
            except InvalidCatalog:
                pass
        require(matches == 1, 'must match exactly one allowed shape')
    if 'const' in schema:
        require(type(value) is type(schema['const']) and value == schema['const'],
                f"expected {schema['const']!r}")
    if 'enum' in schema:
        require(any(type(value) is type(item) and value == item
                    for item in schema['enum']), 'unknown value')
    types = {'object': dict, 'array': list, 'string': str, 'boolean': bool}
    if 'type' in schema:
        require(schema['type'] in types, 'unsupported schema type')
        require(type(value) is types[schema['type']], f"expected {schema['type']}")
    if isinstance(value, dict):
        properties = schema.get('properties', {})
        require(set(schema.get('required', [])) <= value.keys(), 'missing required fields')
        for key, item in value.items():
            if 'propertyNames' in schema:
                validate_node(key, schema['propertyNames'], root, location)
            if key in properties:
                validate_node(item, properties[key], root, f'{location}.{key}')
            elif schema.get('additionalProperties') is False:
                require(False, f'unknown field {key}')
            elif isinstance(schema.get('additionalProperties'), dict):
                validate_node(item, schema['additionalProperties'], root, f'{location}.{key}')
    if isinstance(value, list):
        require(len(value) >= schema.get('minItems', 0), 'too few entries')
        if 'items' in schema:
            for index, item in enumerate(value):
                validate_node(item, schema['items'], root, f'{location}[{index}]')
    if isinstance(value, str):
        require(len(value) >= schema.get('minLength', 0), 'string too short')
        if 'pattern' in schema:
            require(re.search(schema['pattern'], value) is not None, 'invalid string pattern')
        if schema.get('format') == 'uri':
            require(safe_url(value), 'expected HTTP(S) URL without credentials')
        if schema.get('format') == 'relative-path':
            require(safe_path(value), 'unsafe relative path')


def unique_ids(items, location):
    identifiers = [item['id'] for item in items]
    if len(identifiers) != len(set(identifiers)):
        raise InvalidCatalog(f'{location}: duplicate IDs')


def validate(document, schema=None):
    schema = read_json(SCHEMA) if schema is None else schema
    check_schema(schema)
    validate_node(document, schema, schema)
    unique_ids(document['connectors'], 'connectors')
    for connector in document['connectors']:
        unique_ids(connector['servers'], connector['id'] + '.servers')
        unique_ids(connector.get('skills', []), connector['id'] + '.skills')
        for server in connector['servers']:
            headers = server.get('headers', {})
            if len({key.lower() for key in headers}) != len(headers):
                raise InvalidCatalog('duplicate case-insensitive header names')
            values = list(headers.values())
            placeholders = [name for value in values
                            for name in re.findall(r'\$\{([^}]*)\}', value)]
            if any('${' in re.sub(r'\$\{[^}]*\}', '', value) for value in values):
                raise InvalidCatalog('malformed header placeholder')
            if server['auth']['kind'] == 'api_key':
                if not placeholders or set(placeholders) != {'credential'}:
                    raise InvalidCatalog('API-key headers must reference only ${credential}')
            elif placeholders:
                raise InvalidCatalog('credential placeholders require api_key auth')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('paths', nargs='*', type=Path,
                        help='Catalog documents; defaults to synthetic test fixture')
    args = parser.parse_args()
    paths = args.paths or [ROOT / 'tests/catalog/example.json']
    try:
        schema = read_json(SCHEMA)
        for path in paths:
            validate(read_json(path), schema)
            print(f'Catalog valid: {path}')
    except (OSError, ValueError) as error:
        print(f'Catalog validation failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
