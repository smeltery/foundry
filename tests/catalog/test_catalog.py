"""Offline schema and semantic checks using synthetic catalog records only."""
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('catalog_check', ROOT / 'scripts/checks/catalog.py')
catalog = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(catalog)


class CatalogTests(unittest.TestCase):
    def setUp(self):
        self.document = catalog.read_json(Path(__file__).with_name('example.json'))
        self.connector = self.document['connectors'][0]
        self.server = self.connector['servers'][0]

    def test_synthetic_fixture(self):
        self.assertTrue(self.connector['fixture'])
        catalog.validate(self.document)

    def test_auth_and_adapter_variants(self):
        for auth in ('none', 'oauth'):
            with self.subTest(auth=auth):
                self.server['auth'] = {'kind': auth}
                self.server['headers'] = {'X-Fixture': 'true'}
                self.server['endpoint'] = {'kind': 'adapter', 'id': 'sample', 'state': 'unavailable'}
                self.connector['icon'] = {'kind': 'local', 'path': 'assets/sample.svg'}
                catalog.validate(self.document)

    def test_duplicate_ids(self):
        for items in (self.document['connectors'], self.connector['servers'], self.connector['skills']):
            with self.subTest(items=items[0]['id']):
                items.append(copy.deepcopy(items[0]))
                with self.assertRaisesRegex(catalog.InvalidCatalog, 'duplicate IDs'):
                    catalog.validate(self.document)
                items.pop()

    def test_unsafe_paths(self):
        for path in ('../icon.svg', '/icon.svg', 'C:/icon.svg', 'assets/../icon.svg',
                     'assets\\icon.svg', '%2e%2e/icon.svg', './icon.svg', 'assets//icon.svg'):
            with self.subTest(path=path):
                self.connector['icon'] = {'kind': 'local', 'path': path}
                with self.assertRaises(catalog.InvalidCatalog):
                    catalog.validate(self.document)

    def test_missing_or_invalid_auth_placeholder(self):
        for headers in ({}, {'Authorization': 'Bearer literal'},
                        {'Authorization': 'Bearer ${unknown}'}, {'Authorization': '${credential'}):
            with self.subTest(headers=headers):
                self.server['headers'] = headers
                with self.assertRaises(catalog.InvalidCatalog):
                    catalog.validate(self.document)

    def test_header_variants(self):
        for prefix in ('Bearer', 'Token', 'Apikey'):
            self.server['headers'] = {'Authorization': prefix + ' ${credential}'}
            catalog.validate(self.document)

    def test_unknown_adapter_state(self):
        self.server['endpoint'] = {'kind': 'adapter', 'id': 'sample', 'state': 'ready'}
        with self.assertRaises(catalog.InvalidCatalog):
            catalog.validate(self.document)

    def test_invalid_revision(self):
        self.connector['skills'][0]['revision'] = 'main'
        with self.assertRaises(catalog.InvalidCatalog):
            catalog.validate(self.document)

    def test_invalid_headers_and_urls(self):
        for headers in ({'Authorization': '${credential}\r\nInjected: true'},
                        {'Authorization': '${credential}', 'authorization': 'other'}):
            self.server['headers'] = headers
            with self.assertRaises(catalog.InvalidCatalog):
                catalog.validate(self.document)
        self.server['headers'] = {'Authorization': '${credential}'}
        for url in ('https://', 'file:///tmp/a', 'https://user:secret@provider.example/mcp'):
            self.server['endpoint']['url'] = url
            with self.assertRaises(catalog.InvalidCatalog):
                catalog.validate(self.document)

    def test_unknown_fields_and_schema_keywords(self):
        self.connector['surprise'] = True
        with self.assertRaises(catalog.InvalidCatalog):
            catalog.validate(self.document)
        del self.connector['surprise']
        schema = catalog.read_json(catalog.SCHEMA)
        schema['not'] = {}
        with self.assertRaisesRegex(catalog.InvalidCatalog, 'unsupported schema'):
            catalog.validate(self.document, schema)

    def test_duplicate_json_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'duplicate.json'
            path.write_text('{"schemaVersion": 1, "schemaVersion": 1}')
            with self.assertRaisesRegex(catalog.InvalidCatalog, 'duplicate JSON key'):
                catalog.read_json(path)


if __name__ == '__main__':
    unittest.main()
