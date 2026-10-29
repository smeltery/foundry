"""Boundary tests independent of any client descriptor design."""
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

from files import (InvalidPackage, archive_inventory, compare_archive, contained,
                   frontmatter, inventory, metadata, read_json)


class FileTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve() / 'package'
        self.root.mkdir()
        (self.root / '.client').mkdir()
        (self.root / '.client/plugin.json').write_text('{"name":"sample"}\n')
        (self.root / 'guide.md').write_text('Original fixture guidance.\n')

    def test_relative_reference_and_internal_document_link(self):
        self.assertEqual(contained(self.root, './.client/plugin.json'),
                         self.root / '.client/plugin.json')
        self.assertEqual(contained(self.root, '../guide.md',
                                   base=self.root / '.client', parents=True),
                         self.root / 'guide.md')

    def test_unsafe_or_absent_references(self):
        for value in ('../outside', '/absolute', 'C:/file', 'a\\b',
                      '%2e%2e/file', './missing', 'a//b', 'guide.md\x00',
                      'file:///tmp/file', '../../escape'):
            with self.subTest(value=value), self.assertRaises(InvalidPackage):
                contained(self.root, value)
        with self.assertRaises(InvalidPackage):
            contained(self.root, '../../escape', base=self.root / '.client', parents=True)

    def test_symlinks_do_not_hide_missing_packaged_files(self):
        (self.root / 'alias').symlink_to(self.root / 'guide.md')
        with self.assertRaises(InvalidPackage):
            contained(self.root, 'alias')
        with self.assertRaises(InvalidPackage):
            inventory(self.root)

    def test_strict_json(self):
        path = self.root / 'data.json'
        for value in ('{"name":1,"name":2}', '{"x":NaN}', '[]',
                      '\ufeff{}', '{"x":Infinity}', '{'):
            path.write_text(value)
            with self.subTest(value=value), self.assertRaises(InvalidPackage):
                read_json(path)
        path.write_text('{"version":"1.0.0","nested":{"enabled":false}}')
        self.assertEqual(read_json(path)['nested'], {'enabled': False})

    def test_authored_metadata_subset(self):
        self.assertEqual(metadata('interface:\n  display_name: Foundry\n  enabled: false'),
                         {'interface': {'display_name': 'Foundry', 'enabled': False}})
        self.assertEqual(metadata('description: "A task: with punctuation"'),
                         {'description': 'A task: with punctuation'})
        for value in ('a: x\na: y', 'a:\n    b: x', 'a: &alias', 'a: null',
                      'a: !tag value', 'a: [one]', '\ta: x', 'a: x # comment'):
            with self.subTest(value=value), self.assertRaises(InvalidPackage):
                metadata(value)

    def test_frontmatter_needs_a_body_and_closing_delimiter(self):
        path = self.root / 'guide.md'
        for value in ('# No header', '---\nname: sample', '---\nname: sample\n---'):
            path.write_text(value)
            with self.subTest(value=value), self.assertRaises(InvalidPackage):
                frontmatter(path)
        path.write_text('---\nname: sample\n---\nRead current state before acting.\n')
        self.assertEqual(frontmatter(path),
                         ({'name': 'sample'}, 'Read current state before acting.'))

    def archive(self, entries, link=False):
        path = Path(self.temp.name) / 'package.tar'
        with tarfile.open(path, 'w') as archive:
            for name, data in entries:
                entry = tarfile.TarInfo(name)
                entry.size = len(data)
                if link:
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = '/outside'
                archive.addfile(entry, io.BytesIO(data))
        return path

    def test_archive_retains_hidden_files_and_exact_bytes(self):
        expected = inventory(self.root)
        self.assertIn('.client/plugin.json', expected)
        compare_archive(self.archive(expected.items()), expected)
        visible = [(name, data) for name, data in expected.items() if not name.startswith('.')]
        with self.assertRaises(InvalidPackage):
            compare_archive(self.archive(visible), expected)
        changed = dict(expected, **{'guide.md': b'Changed instructions.'})
        with self.assertRaises(InvalidPackage):
            compare_archive(self.archive(changed.items()), expected)

    def test_archive_escape_duplicate_and_link_rejection(self):
        for name in ('../escape', '/absolute', 'C:/file', './guide.md', 'a//b'):
            with self.subTest(name=name), self.assertRaises(InvalidPackage):
                archive_inventory(self.archive([(name, b'x')]))
        with self.assertRaises(InvalidPackage):
            archive_inventory(self.archive([('same', b'x'), ('same', b'y')]))
        with self.assertRaises(InvalidPackage):
            archive_inventory(self.archive([('link', b'')], link=True))


if __name__ == '__main__':
    unittest.main()
