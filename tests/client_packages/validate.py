#!/usr/bin/env python3
"""Validate an authored source tree or explicitly prepared local distribution."""
import argparse
from pathlib import Path
import sys

from descriptors import PACKAGE, connections, descriptors, discovery
from files import InvalidPackage, contained, fields, inventory, read_json, require


def package_inventory(root, package, prepared):
    document = read_json(contained(package, 'files.json'))
    fields(document, ('version', 'files'))
    require(type(document['version']) is int and document['version'] == 1, 'inventory version')
    names = document['files']
    require(isinstance(names, list) and names and all(isinstance(n, str) for n in names),
            'inventory file list required')
    require(len(set(names)) == len(names) and 'files.json' in names, 'duplicate or incomplete inventory')
    for name in names:
        require(not name.startswith('./') and not name.endswith('/'), 'canonical inventory path required')
        contained(package, name)
    expected = set(names)
    if prepared:
        expected.update(('bin/f5y', 'mcp.json', '.mcp.json', '.cursor-plugin/mcp.json'))
    entries = list(package.rglob('*'))
    require(all(not p.is_symlink() for p in entries), 'symbolic link in package')
    actual = {p.relative_to(package).as_posix() for p in entries if not p.is_dir()}
    require(actual == expected, 'unlisted package content or missing inventory entry')
    require('LICENSE' in names, 'package license must be inventoried')
    require(contained(root, 'LICENSE').read_bytes() == contained(package, 'LICENSE').read_bytes(),
            'package license differs from root')
    if prepared:
        notices = contained(root, 'THIRD_PARTY_NOTICES', directory=True)
        require(bool(inventory(notices)), 'nonempty applicable notice packet required')


def validate(root, prepared=False):
    root = Path(root).resolve(strict=True)
    package = contained(root, PACKAGE, directory=True)
    package_inventory(root, package, prepared)
    discovery(root)
    documents = descriptors(package, prepared)
    connections(root, package, prepared)
    from content import validate_content
    validate_content(package, documents)
    require(package.is_dir(), 'package root absent')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--prepared', action='store_true')
    args = parser.parse_args()
    try:
        validate(args.root, args.prepared)
    except (InvalidPackage, OSError) as error:
        print(f'Client package invalid: {error}', file=sys.stderr)
        return 1
    print('Client package structure validated; installed-client execution is not tested.')
    return 0


if __name__ == '__main__':
    sys.exit(main())
