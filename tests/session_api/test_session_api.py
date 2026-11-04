"""Offline checks for the neutral session API vectors and their validator."""
import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest

import validate_vectors as vectors

HERE = Path(__file__).resolve().parent


class SessionApiVectorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name) / 'session_api'
        shutil.copytree(HERE, self.directory, ignore=shutil.ignore_patterns('__pycache__'))
        self.catalog = vectors.read_json(self.directory / 'catalog.json')

    def load(self, name):
        return vectors.read_json(self.directory / 'vectors' / name)

    def save(self, name, document):
        (self.directory / 'vectors' / name).write_text(json.dumps(document))

    def assertInvalid(self, pattern):
        with self.assertRaisesRegex(vectors.InvalidVectors, pattern):
            vectors.validate(vectors.ROOT, self.directory)

    def mutate_case(self, name, index, change, pattern):
        """Apply one change to a copy of a case, expect rejection, then restore."""
        original = self.load(name)
        document = copy.deepcopy(original)
        change(document['cases'][index])
        self.save(name, document)
        try:
            self.assertInvalid(pattern)
        finally:
            self.save(name, original)

    def test_repository_vectors_are_valid(self):
        self.assertGreater(vectors.validate(), 0)

    def test_every_audit_row_has_a_section_and_vectors(self):
        rows = self.catalog['rows']
        self.assertEqual(len(rows), 51)
        for row, entry in rows.items():
            with self.subTest(row=row):
                self.assertTrue(entry['operations'] or entry.get('routed'))

    def test_duplicate_json_keys_are_rejected(self):
        (self.directory / 'vectors/extra.json').write_text('{"format": 1, "format": 1}')
        self.assertInvalid('duplicate JSON key')

    def test_unknown_error_class_is_rejected(self):
        self.mutate_case('create.json', 3, lambda case: case['expect'].update(error='teapot'),
                         'unknown error class')

    def test_error_result_requires_error_class(self):
        self.mutate_case('create.json', 3, lambda case: case['expect'].pop('error'),
                         'exactly one error class')

    def test_implemented_case_needs_existing_test(self):
        self.mutate_case('create.json', 0, lambda case: case.pop('evidence'), 'need evidence')
        missing = ['crates/store/tests/durability.rs::no_such_test']
        self.mutate_case('create.json', 0, lambda case: case.update(evidence=missing),
                         'evidence test does not exist')
        unsafe = ['crates/../../outside.rs::name']
        self.mutate_case('create.json', 0, lambda case: case.update(evidence=unsafe), 'unsafe')

    def test_unimplemented_case_cannot_cite_evidence(self):
        cited = ['crates/store/tests/durability.rs::concurrent_create_and_attempt_claim_have_one_winner']
        self.mutate_case('create.json', 1, lambda case: case.update(evidence=cited), 'only they')

    def test_limit_outcomes_follow_the_catalog(self):
        self.mutate_case('create.json', 7, lambda case: case['expect'].update(error='invalid_request'),
                         'must fail with payload_too_large')
        self.mutate_case('create.json', 7, lambda case: case['limit'].update(value=10),
                         'within bounds')
        self.mutate_case('read.json', 11, lambda case: case['expect'].update(effective_page=500),
                         'capped limit')

    def test_lifecycle_rules(self):
        self.mutate_case('lifecycle.json', 3, lambda case: case['expect'].pop('outcome'),
                         'exit requires an outcome')
        self.mutate_case('lifecycle.json', 4, lambda case: case['expect'].update(outcome='success'),
                         'only exit carries')
        self.mutate_case('lifecycle.json', 4, lambda case: case['request'].update(event='admit'),
                         'not in the catalog')
        self.mutate_case('continuation.json', 1, lambda case: case['expect'].update(lifecycle='queued'),
                         'error cannot become queued')

    def test_rows_must_cover_their_operation(self):
        self.mutate_case('create.json', 0, lambda case: case['rows'].append('API-X031'),
                         'does not cover')

    def test_addresses_and_paths_stay_neutral(self):
        address = 'https://real.invalid/x'
        self.mutate_case('artifacts.json', 3, lambda case: case['request'].update(address=address),
                         'reserved .example')
        self.mutate_case('create.json', 1, lambda case: case['request'].update(directory='/home/user'),
                         'under /workspace/')

    def test_uncovered_row_and_document_drift_fail(self):
        document = self.load('errors.json')
        self.save('errors.json', dict(document, cases=[]))
        self.assertInvalid('no cases')
        (self.directory / 'vectors/errors.json').unlink()
        self.assertInvalid(r"rows without vectors: \['API-008'\]")
        catalog = copy.deepcopy(self.catalog)
        catalog['rows']['API-001']['section'] = 'missing-section'
        (self.directory / 'catalog.json').write_text(json.dumps(catalog))
        self.assertInvalid('API-001: no section')

    def test_heading_slugs_match_document_links(self):
        self.assertEqual(vectors.slug('Isolated, prestart and experimental options'),
                         'isolated-prestart-and-experimental-options')
        found = vectors.sections('# A\n\n## B\ntext\n### C\n## D\n```\n## not a heading\n```\n')
        self.assertIn('text', found['b'])
        self.assertIn('### C', found['b'])
        self.assertNotIn('not-a-heading', found)


if __name__ == '__main__':
    unittest.main()
