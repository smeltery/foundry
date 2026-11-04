"""Offline checks for the compatibility row registry and its checker."""
import copy
import importlib.util
import io
import json
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('compatibility_check', ROOT / 'scripts/checks/compatibility.py')
compatibility = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(compatibility)

SOURCES = {
    'crates/sample/tests/flow.rs': '#[test]\nfn creates_session() {}\n#[tokio::test]\nasync fn generic_flow<T>() {}\n',
    'tests/sample/test_flow.py': 'class Flow:\n    def test_creates_session(self):\n        pass\n',
    'packages/sample/test/flow.test.ts': 'test("creates a session (inclusive)", async () => {});\n',
    'tests/e2e/session/flow.rs': 'fn end_to_end_session() {}\n',
}
DOCUMENT = {
    'format': 1,
    'rows': {
        'API-001': {'tests': [
            {'kind': 'integration', 'path': 'crates/sample/tests/flow.rs', 'function': 'creates_session'},
            {'kind': 'integration', 'path': 'crates/sample/tests/flow.rs', 'function': 'generic_flow'},
        ]},
        'API-X042': {'tests': [
            {'kind': 'unit', 'path': 'tests/sample/test_flow.py', 'function': 'test_creates_session'},
        ], 'baseline': 'sha256:' + '0' * 64},
        'AG-07': {'tests': [
            {'kind': 'unit', 'path': 'packages/sample/test/flow.test.ts',
             'function': 'creates a session (inclusive)'},
        ]},
        'CLI-003': {'tests': [
            {'kind': 'e2e', 'path': 'tests/e2e/session/flow.rs', 'function': 'end_to_end_session'},
        ]},
    },
}


class CompatibilityRegistryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        for relative, text in SOURCES.items():
            (self.root / relative).parent.mkdir(parents=True, exist_ok=True)
            (self.root / relative).write_text(text)
        self.rows = self.root / 'tests/compatibility/rows'
        self.rows.mkdir(parents=True)
        self.document = copy.deepcopy(DOCUMENT)

    def report(self):
        (self.rows / 'sample.json').write_text(json.dumps(self.document))
        return compatibility.build_report(self.root, self.rows)

    def assertInvalid(self, pattern):
        with self.assertRaisesRegex(compatibility.InvalidRegistry, pattern):
            self.report()

    def test_repository_registry_resolves_every_citation(self):
        report = compatibility.build_report()
        self.assertEqual(report['missing'], [])
        self.assertTrue(all(entry['resolved'] for entry in report['rows'].values()))

    def test_rust_python_typescript_and_e2e_citations_resolve(self):
        report = self.report()
        self.assertEqual(report['missing'], [])
        self.assertEqual(sorted(report['rows']), ['AG-07', 'API-001', 'API-X042', 'CLI-003'])
        self.assertEqual(report['rows']['API-X042']['baseline'], 'sha256:' + '0' * 64)
        self.assertEqual(len(report['rows']['API-001']['resolved']), 2)

    def test_neutral_id_shapes_are_accepted(self):
        for row in ('API-099', 'API-X100', 'AG-12', 'CLI-120', 'NET-007', 'NET-X007', 'CAT-001', 'REL-010'):
            with self.subTest(row=row):
                self.document['rows'] = {row: DOCUMENT['rows']['API-001']}
                self.assertIn(row, self.report()['rows'])

    def test_malformed_row_ids_are_rejected(self):
        for row in ('API-1', 'api-001', 'AG-123', 'NIC-130', 'GAP-001', 'API-001 extra', 'XYZ-001'):
            with self.subTest(row=row):
                self.document['rows'] = {row: DOCUMENT['rows']['API-001']}
                self.assertInvalid('not a neutral ID')

    def test_missing_files_and_functions_are_reported(self):
        tests = self.document['rows']['API-001']['tests']
        tests.append({'kind': 'integration', 'path': 'crates/sample/tests/absent.rs', 'function': 'absent'})
        tests.append({'kind': 'integration', 'path': 'crates/sample/tests/flow.rs', 'function': 'absent'})
        self.document['rows']['API-X042']['tests'][0]['function'] = 'test_absent'
        self.document['rows']['AG-07']['tests'][0]['function'] = 'creates a session'
        problems = sorted((gap['row'], gap['problem']) for gap in self.report()['missing'])
        self.assertEqual(problems, [
            ('AG-07', 'test function does not exist'),
            ('API-001', 'file does not exist'),
            ('API-001', 'test function does not exist'),
            ('API-X042', 'test function does not exist'),
        ])

    def test_future_e2e_citation_is_missing_until_the_file_exists(self):
        self.document['rows']['CLI-003']['tests'][0]['path'] = 'tests/e2e/later/flow.rs'
        self.assertEqual([gap['problem'] for gap in self.report()['missing']], ['file does not exist'])

    def test_addresses_and_prose_fields_are_rejected(self):
        self.document['rows']['API-001']['tests'][0]['function'] = 'see https://host.example/a'
        self.assertInvalid('addresses are not allowed')
        self.document = copy.deepcopy(DOCUMENT)
        self.document['rows']['API-001']['title'] = 'Create a session'
        self.assertInvalid('missing or unknown keys')
        self.document = copy.deepcopy(DOCUMENT)
        self.document['rows']['API-001']['tests'][0]['source'] = 'elsewhere'
        self.assertInvalid('missing or unknown keys')
        self.document = copy.deepcopy(DOCUMENT)
        self.document['note'] = 'free text'
        self.assertInvalid('missing or unknown keys')

    def test_free_text_function_names_are_rejected(self):
        for path, function in (('crates/sample/tests/flow.rs', 'Creates Session'),
                               ('tests/sample/test_flow.py', 'creates_session'),
                               ('packages/sample/test/flow.test.ts', 'has "quotes"'),
                               ('packages/sample/test/flow.test.ts', 'x' * 200)):
            with self.subTest(function=function):
                self.document['rows'] = {'API-001': {'tests': [
                    {'kind': 'unit', 'path': path, 'function': function}]}}
                self.assertInvalid('is not a .* test name')

    def test_unsafe_or_unsupported_paths_are_rejected(self):
        for path, pattern in (('../outside/flow.rs', 'relative under'),
                              ('/crates/sample/flow.rs', 'relative under'),
                              ('crates/../tests/flow.rs', 'unsafe'),
                              ('crates//sample/flow.rs', 'unsafe'),
                              ('docs/flow.rs', 'relative under'),
                              ('crates/sample/flow.md', 'must be a'),
                              ('tests/e2e/flow.rs', 'e2e citations')):
            with self.subTest(path=path):
                self.document['rows'] = {'API-001': {'tests': [
                    {'kind': 'unit', 'path': path, 'function': 'flow'}]}}
                self.assertInvalid(pattern)

    def test_e2e_kind_requires_the_e2e_tree(self):
        self.document['rows']['CLI-003']['tests'][0]['path'] = 'crates/sample/tests/flow.rs'
        self.assertInvalid('e2e citations')

    def test_kind_baseline_and_shape_rules(self):
        cases = (
            (('API-001', 'tests', 0, 'kind'), 'manual', 'kind must be one of'),
            (('API-X042', 'baseline'), 'sha256:ABC', 'baseline must be'),
            (('API-X042', 'baseline'), 'version 1', 'baseline must be'),
            (('API-001', 'tests'), [], 'nonempty list'),
        )
        for keys, value, pattern in cases:
            with self.subTest(keys=keys):
                self.document = copy.deepcopy(DOCUMENT)
                target = self.document['rows']
                for key in keys[:-1]:
                    target = target[key]
                target[keys[-1]] = value
                self.assertInvalid(pattern)

    def test_duplicates_are_rejected(self):
        tests = self.document['rows']['API-001']['tests']
        tests.append(dict(tests[0]))
        self.assertInvalid('duplicate citation')
        self.document = copy.deepcopy(DOCUMENT)
        (self.rows / 'other.json').write_text(json.dumps(
            {'format': 1, 'rows': {'API-001': DOCUMENT['rows']['API-001']}}))
        self.assertInvalid('registered more than once')
        (self.rows / 'other.json').unlink()
        (self.rows / 'sample.json').write_text('{"format": 1, "format": 1, "rows": {}}')
        with self.assertRaisesRegex(compatibility.InvalidRegistry, 'duplicate JSON key'):
            compatibility.build_report(self.root, self.rows)

    def test_symlinked_test_file_is_not_evidence(self):
        outside = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: (outside / 'flow.rs').unlink(missing_ok=True) or outside.rmdir())
        (outside / 'flow.rs').write_text('fn creates_session() {}\n')
        link = self.root / 'crates/sample/tests/linked.rs'
        link.symlink_to(outside / 'flow.rs')
        self.document['rows']['API-001']['tests'][0]['path'] = 'crates/sample/tests/linked.rs'
        self.assertEqual([gap['problem'] for gap in self.report()['missing']], ['file does not exist'])

    def test_command_writes_report_and_fails_on_missing_citations(self):
        self.report()
        target = self.root / 'report.json'
        arguments = ['--root', str(self.root), '--rows', str(self.rows), '--report', str(target)]
        with redirect_stderr(io.StringIO()):
            self.assertEqual(compatibility.main(arguments), 0)
        self.assertEqual(json.loads(target.read_text())['missing'], [])
        self.document['rows']['AG-07']['tests'][0]['function'] = 'absent test'
        self.report()
        stderr, stdout = io.StringIO(), io.StringIO()
        with redirect_stderr(stderr), redirect_stdout(stdout):
            self.assertEqual(compatibility.main(arguments[:4]), 1)
        self.assertEqual(json.loads(stdout.getvalue())['missing'][0]['row'], 'AG-07')
        self.assertIn('1 missing', stderr.getvalue())
        (self.rows / 'sample.json').write_text('{"format": 2, "rows": {}}')
        with redirect_stderr(io.StringIO()):
            self.assertEqual(compatibility.main(arguments), 1)


if __name__ == '__main__':
    unittest.main()
