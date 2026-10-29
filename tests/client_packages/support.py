"""Test inputs are local scratch files; preparation never runs their executable."""
import importlib.util
import json
import os
from pathlib import Path
import shutil


def repository():
    return Path(os.environ.get('FOUNDRY_CLIENT_PACKAGE_ROOT',
                              Path(__file__).resolve().parents[2])).resolve()


def preparer():
    path = repository() / 'scripts/clients/prepare.py'
    spec = importlib.util.spec_from_file_location('client_preparation', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write_json(path, document):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, indent=2) + '\n')


def package_copy(destination):
    root = repository()
    destination.mkdir()
    shutil.copy2(root / 'LICENSE', destination / 'LICENSE')
    for relative in ('.agents/plugins/marketplace.json', '.claude-plugin/marketplace.json',
                     '.cursor-plugin/marketplace.json'):
        path = destination / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / relative, path)
    shutil.copytree(root / 'plugins/foundry', destination / 'plugins/foundry')
    return destination


def selected_inputs(root):
    binary = root / 'selected-cli'
    marker = root / 'must-not-run'
    binary.write_text(f'#!/bin/sh\ntouch "{marker}"\nexit 71\n')
    binary.chmod(0o755)
    database = root / 'selected.db'
    database.write_bytes(b'Original inert database fixture; do not open or change.\n')
    notices = root / 'notices'
    notices.mkdir()
    (notices / 'NOTICE.txt').write_text('Original synthetic notice packet for testing.\n')
    (notices / '.hidden-notice').write_text('Retain this notice verbatim.\n')
    return binary, database, notices, marker
