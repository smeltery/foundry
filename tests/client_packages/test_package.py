"""Check real shipped content and isolated mutations, without a client install."""
import copy
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

from content import svg
from descriptors import CONFIGS, DISCOVERY, PACKAGE
from files import InvalidPackage, compare_archive, inventory, read_json
from support import package_copy, preparer, repository, selected_inputs, write_json
from validate import validate


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = package_copy(self.root / 'source')
        self.package = self.source / PACKAGE

    def test_actual_repository_package(self):
        # Never skipped: the standard Python check gates the delivered real tree.
        validate(repository())

    def test_isolated_packaged_root_and_hidden_archive_inventory(self):
        validate(self.source)
        expected = inventory(self.source)
        archive_path = self.root / 'client-package.tar'
        for omit_hidden in (False, True):
            with tarfile.open(archive_path, 'w') as archive:
                for name, data in expected.items():
                    if omit_hidden and any(part.startswith('.') for part in Path(name).parts):
                        continue
                    member = tarfile.TarInfo(name)
                    member.size = len(data)
                    archive.addfile(member, io.BytesIO(data))
            if omit_hidden:
                with self.assertRaises(InvalidPackage):
                    compare_archive(archive_path, expected)
            else:
                compare_archive(archive_path, expected)

    def changed_json(self, path, change, prepared=False, root=None):
        original = read_json(path)
        document = copy.deepcopy(original)
        change(document)
        write_json(path, document)
        try:
            with self.assertRaises(InvalidPackage):
                validate(root or self.source, prepared=prepared)
        finally:
            write_json(path, original)

    def test_discovery_does_not_resolve_from_hidden_manifest_folder(self):
        for relative in DISCOVERY:
            def change(document):
                entry = document['plugins'][0]
                if isinstance(entry['source'], dict):
                    entry['source']['path'] = '../../plugins/foundry'
                else:
                    entry['source'] = '../../plugins/foundry'
            with self.subTest(relative=relative):
                self.changed_json(self.source / relative, change)

    def test_versions_schema_and_client_fields(self):
        for client in ('claude', 'codex', 'cursor'):
            self.changed_json(self.package / f'.{client}-plugin/plugin.json',
                              lambda d: d.update(version='9.0.0'))
        self.changed_json(self.package / 'plugin.json', lambda d: d.update({'$schema': 'invalid'}))
        self.changed_json(self.package / 'plugin.json', lambda d: d.update(commands='./commands/'))
        self.changed_json(self.package / '.cursor-plugin/plugin.json',
                          lambda d: d.update(commands='../commands/'))
        self.changed_json(self.package / '.codex-plugin/plugin.json',
                          lambda d: d.update(skills=[]))

    def test_missing_workflow_asset_and_hidden_descriptor(self):
        for relative in ('commands/f5y-fix-ci.md', 'assets/logo-light.svg',
                         '.claude-plugin/plugin.json'):
            path = self.package / relative
            data = path.read_bytes()
            path.unlink()
            try:
                with self.subTest(relative=relative), self.assertRaises(InvalidPackage):
                    validate(self.source)
            finally:
                path.write_bytes(data)

    def test_instruction_capability_and_variable_mutations(self):
        path = self.package / 'skills/foundry/SKILL.md'
        original = path.read_text()
        for text in (original + '\nCall `session_run` now.\n',
                     original + '\nUse ${UNDECLARED_TOKEN}.\n',
                     original.replace('neither starts nor resumes', 'starts and resumes')):
            path.write_text(text)
            with self.assertRaises(InvalidPackage):
                validate(self.source)
        path.write_text(original)
        rule = self.package / 'rules/foundry-handoff.mdc'
        rule.write_text(rule.read_text().replace('alwaysApply: false', 'alwaysApply: true'))
        with self.assertRaises(InvalidPackage):
            validate(self.source)

    def test_escaped_links_and_license_drift(self):
        command = self.package / 'commands/f5y-ask.md'
        original = command.read_text()
        command.write_text(original + '\n[Outside](../../../LICENSE)\n')
        with self.assertRaises(InvalidPackage):
            validate(self.source)
        command.write_text(original)
        (self.package / 'LICENSE').write_text('Changed license fixture.\n')
        with self.assertRaises(InvalidPackage):
            validate(self.source)

    def test_inventory_types_and_unique_files(self):
        path = self.package / 'files.json'
        for change in (lambda d: d.update(version=True),
                       lambda d: d['files'].append(d['files'][0]),
                       lambda d: d['files'].append('../LICENSE'),
                       lambda d: d['files'].remove('files.json')):
            self.changed_json(path, change)

    def test_svg_rejects_scripts_external_refs_and_invalid_geometry(self):
        path = self.package / 'assets/logo-light.svg'
        original = path.read_text()
        for body in ('<script/>', '<image href="https://invalid.example/image"/>',
                     '<path onclick="alert(1)"/>'):
            path.write_text(original.replace('</svg>', body + '</svg>'))
            with self.assertRaisesRegex(InvalidPackage, 'active|external'):
                svg(path)
        path.write_text(original.replace('viewBox="0 0 128 128"', 'viewBox="0 0 128 64"'))
        with self.assertRaisesRegex(InvalidPackage, 'square'):
            svg(path)
        path.write_text(original.replace('fill="#F5F2EA"', 'fill="none"'))
        with self.assertRaisesRegex(InvalidPackage, 'opaque'):
            svg(path)

    def prepared(self):
        binary, database, notices, _ = selected_inputs(self.root)
        output = self.root / 'prepared'
        preparer().prepare(self.source, binary, database, output, notices)
        return output

    def test_prepared_transport_and_selected_input_mismatches(self):
        output = self.prepared()
        validate(output, prepared=True)
        for name in CONFIGS:
            path = output / PACKAGE / name
            for change in (lambda d: d['mcpServers']['foundry'].update(command='f5y'),
                           lambda d: d['mcpServers']['foundry'].update(url='https://invalid.example'),
                           lambda d: d['mcpServers']['foundry'].update(args=['mcp']),
                           lambda d: d['mcpServers']['foundry'].update(env={'TOKEN': '${TOKEN}'})):
                with self.subTest(name=name):
                    self.changed_json(path, change, prepared=True, root=output)
        for client in ('claude', 'codex', 'cursor'):
            self.changed_json(output / PACKAGE / f'.{client}-plugin/plugin.json',
                              lambda d: d.pop('mcpServers'), prepared=True, root=output)
        (output / PACKAGE / 'bin/f5y').chmod(0o755)
        (output / PACKAGE / 'bin/f5y').write_bytes(b'Changed executable')
        with self.assertRaises(InvalidPackage):
            validate(output, prepared=True)

    def test_modes_are_explicit_and_entrypoint_exits(self):
        output = self.prepared()
        with self.assertRaises(InvalidPackage):
            validate(self.source, prepared=True)
        with self.assertRaises(InvalidPackage):
            validate(output)
        program = Path(__file__).with_name('validate.py')
        for root, extra, expected in ((self.source, [], 0), (output, ['--prepared'], 0),
                                      (self.source, ['--prepared'], 1)):
            result = subprocess.run([sys.executable, str(program), '--root', str(root), *extra],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, expected, result.stderr)
            self.assertNotIn('Traceback', result.stderr)


if __name__ == '__main__':
    unittest.main()
