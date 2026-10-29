"""Preparation boundaries: selected bytes only, no execution or settings writes."""
from pathlib import Path
import os
import tempfile
import unittest

from descriptors import CONFIGS, MCP_SCHEMA
from files import read_json
from support import package_copy, preparer, selected_inputs, write_json
from validate import validate


class PrepareTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = package_copy(self.root / 'source')
        self.package = self.source / 'plugins/foundry'
        self.binary, self.database, self.notices, self.marker = selected_inputs(self.root)
        self.output = self.root / 'export'
        self.module = preparer()

    def prepare(self, **changes):
        arguments = dict(root=self.source, binary=self.binary, database=self.database,
                         output=self.output, notices=self.notices)
        arguments.update(changes)
        return self.module.prepare(**arguments)

    def test_actual_prepared_package_and_selected_bytes(self):
        database_bytes = self.database.read_bytes()
        database_time = self.database.stat().st_mtime_ns
        self.prepare()
        validate(self.output, prepared=True)
        package = self.output / 'plugins/foundry'
        binary = package / 'bin/f5y'
        self.assertEqual(binary.read_bytes(), self.binary.read_bytes())
        self.assertEqual(binary.stat().st_mode & 0o777, 0o555)
        self.assertFalse(self.marker.exists())
        self.assertEqual(self.database.read_bytes(), database_bytes)
        self.assertEqual(self.database.stat().st_mtime_ns, database_time)
        self.assertEqual((self.output / 'LICENSE').read_bytes(), (self.source / 'LICENSE').read_bytes())
        for name in ('NOTICE.txt', '.hidden-notice'):
            self.assertEqual((self.output / 'THIRD_PARTY_NOTICES' / name).read_bytes(),
                             (self.notices / name).read_bytes())
        for name in ('.agents/plugins/marketplace.json', '.claude-plugin/marketplace.json',
                     '.cursor-plugin/marketplace.json'):
            self.assertEqual((self.output / name).read_bytes(), (self.source / name).read_bytes())
        args = ['--database', str(self.database), 'mcp']
        self.assertEqual(read_json(package / 'mcp.json'), {
            '$schema': MCP_SCHEMA, 'mcpServers': {'foundry': {
                'type': 'stdio', 'command': './bin/f5y', 'args': args}}})
        self.assertEqual(read_json(package / '.mcp.json'), {'mcpServers': {'foundry': {
            'type': 'stdio', 'command': str(binary), 'args': args}}})
        self.assertEqual(read_json(package / '.cursor-plugin/mcp.json'), {
            'mcpServers': {'foundry': {'command': str(binary), 'args': args}}})
        self.assertTrue(all((package / name).is_file() for name in CONFIGS))

    def test_unlisted_source_is_not_exported(self):
        (self.package / 'private-local.txt').write_text('Unlisted original fixture data.')
        self.prepare()
        self.assertFalse((self.output / 'plugins/foundry/private-local.txt').exists())
        self.assertTrue((self.package / 'private-local.txt').exists())

    def test_absolute_regular_selections_required(self):
        link = self.root / 'alias'
        link.symlink_to(self.binary)
        for field, value in (('binary', Path('relative')), ('database', Path('relative')),
                             ('binary', link), ('database', link), ('binary', self.root),
                             ('database', self.root / 'absent')):
            with self.subTest(field=field, value=value), self.assertRaises((OSError, ValueError)):
                self.prepare(**{field: value})
            self.assertFalse(self.output.exists())
        self.binary.chmod(0o644)
        with self.assertRaises(ValueError):
            self.prepare()

    def test_existing_output_is_never_overwritten(self):
        self.output.mkdir()
        sentinel = self.output / 'retain'
        sentinel.write_bytes(b'Keep existing output.')
        with self.assertRaises(FileExistsError):
            self.prepare()
        self.assertEqual(sentinel.read_bytes(), b'Keep existing output.')

    def test_source_and_notice_tree_cannot_be_output(self):
        for path in (self.source / 'nested', self.notices / 'nested', Path('relative')):
            with self.subTest(path=path), self.assertRaises(ValueError):
                self.prepare(output=path)

    def test_inventory_rejects_escape_duplicates_symlinks_and_missing_files(self):
        path = self.package / 'files.json'
        original = read_json(path)
        for entry in ('../LICENSE', '/absolute', './plugin.json', 'a//b', 'a\\b',
                      'not-present', 'plugin.json'):
            changed = dict(original, files=original['files'] + [entry])
            write_json(path, changed)
            with self.subTest(entry=entry), self.assertRaises((OSError, ValueError)):
                self.prepare()
            self.assertFalse(self.output.exists())
        write_json(path, original)
        item = self.package / 'plugin.json'
        item.unlink()
        item.symlink_to(self.source / 'LICENSE')
        with self.assertRaises(ValueError):
            self.prepare()

    def test_preparation_inventory_version_and_self_entry_are_required(self):
        path = self.package / 'files.json'
        original = read_json(path)
        invalid = [dict(original, version=True), dict(original, version=2),
                   dict(original, files=[]), dict(original, files=['plugin.json'])]
        for document in invalid:
            write_json(path, document)
            with self.subTest(document=document), self.assertRaises(ValueError):
                self.prepare()
            self.assertFalse(self.output.exists())

    def test_notice_packet_required_and_preserved_without_filtering(self):
        empty = self.root / 'empty-notices'
        empty.mkdir()
        alias = self.root / 'notice-alias'
        alias.symlink_to(self.notices, target_is_directory=True)
        for notice in (empty, alias, Path('relative'), self.binary):
            with self.subTest(notice=notice), self.assertRaises((OSError, ValueError)):
                self.prepare(notices=notice)
            self.assertFalse(self.output.exists())
        (self.notices / 'link').symlink_to(self.binary)
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_failure_after_reservation_cleans_only_new_output(self):
        (self.source / '.claude-plugin/marketplace.json').unlink()
        with self.assertRaises(OSError):
            self.prepare()
        self.assertFalse(self.output.exists())
        self.assertTrue(self.binary.exists())
        self.assertTrue(self.database.exists())
        self.assertTrue(self.notices.exists())

    @unittest.skipUnless(hasattr(os, 'mkfifo'), 'requires filesystem FIFO support')
    def test_special_notice_file_is_rejected_without_reading_it(self):
        os.mkfifo(self.notices / 'pipe')
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertFalse(self.output.exists())


if __name__ == '__main__':
    unittest.main()
