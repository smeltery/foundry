"""Offline checks for the neutral identity vectors and their validator."""
import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest

import validate_vectors as vectors

HERE = Path(__file__).resolve().parent


class IdentityVectorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.directory = self.root / 'tests/identity'
        shutil.copytree(HERE, self.directory, ignore=shutil.ignore_patterns('__pycache__'))
        document = vectors.ROOT / 'docs/architecture/identity.md'
        (self.root / 'docs/architecture').mkdir(parents=True)
        self.document = self.root / 'docs/architecture/identity.md'
        shutil.copy(document, self.document)
        shutil.copytree(vectors.ROOT / 'apps/cli/tests', self.root / 'apps/cli/tests')
        self.catalog = vectors.read_json(self.directory / 'catalog.json')

    def load(self, name):
        return vectors.read_json(self.directory / 'vectors' / name)

    def save(self, name, document):
        (self.directory / 'vectors' / name).write_text(json.dumps(document))

    def assertInvalid(self, pattern):
        with self.assertRaisesRegex(vectors.InvalidVectors, pattern):
            vectors.validate(self.root, self.directory)

    def mutate_case(self, name, case_name, change, pattern):
        """Apply one change to a copy of a named case, expect rejection, then restore."""
        original = self.load(name)
        document = copy.deepcopy(original)
        change(next(case for case in document['cases'] if case['name'] == case_name))
        self.save(name, document)
        try:
            self.assertInvalid(pattern)
        finally:
            self.save(name, original)

    def edit_document(self, old, new, pattern):
        text = self.document.read_text(encoding='utf-8')
        self.assertIn(old, text)
        self.document.write_text(text.replace(old, new), encoding='utf-8')
        self.assertInvalid(pattern)
        self.document.write_text(text, encoding='utf-8')

    def test_repository_vectors_are_valid(self):
        self.assertGreater(vectors.validate(), 0)
        self.assertGreater(vectors.validate(self.root, self.directory), 0)

    def test_all_audit_rows_are_catalogued(self):
        rows = self.catalog['rows']
        self.assertEqual(len(rows), 33)
        self.assertEqual(sum(row.startswith('API-') for row in rows), 31)

    def test_duplicate_json_keys_are_rejected(self):
        (self.directory / 'vectors/extra.json').write_text('{"format": 1, "format": 1}')
        self.assertInvalid('duplicate JSON key')

    def test_error_classes_and_causes(self):
        self.mutate_case('workspaces.json', 'delete_with_wrong_name_is_invalid',
                         lambda case: case['expect'].update(error='teapot'), 'unknown error class')
        self.mutate_case('workspaces.json', 'delete_with_wrong_name_is_invalid',
                         lambda case: case['expect'].update(cause='typo'), 'catalog cause code')
        self.mutate_case('workspaces.json', 'delete_with_wrong_name_is_invalid',
                         lambda case: case['expect'].pop('error'), 'exactly one error class')

    def test_implemented_case_needs_existing_test(self):
        case = 'absent_key_selects_login'
        self.mutate_case('cli.json', case, lambda c: c.pop('evidence'), 'need evidence')
        missing = ['apps/cli/tests/configuration_dispatch.rs::no_such_test']
        self.mutate_case('cli.json', case, lambda c: c.update(evidence=missing), 'test does not exist')
        unsafe = ['apps/../../outside.rs::name']
        self.mutate_case('cli.json', case, lambda c: c.update(evidence=unsafe), 'unsafe')

    def test_check_order_is_enforced(self):
        file = 'authorization.json'
        self.mutate_case(file, 'revoked_key_is_unauthenticated_before_scope',
                         lambda case: case['expect'].update(error='forbidden'), 'requires unauthenticated')
        self.mutate_case(file, 'other_workspace_is_not_found_before_scope',
                         lambda case: case['expect'].update(error='forbidden'), 'requires not_found')
        self.mutate_case(file, 'member_role_cannot_rename',
                         lambda case: case['caller'].update(role='admin'), 'forbidden needs a cause')
        self.mutate_case(file, 'throttled_caller_waits',
                         lambda case: case.pop('caller'), 'gate errors need a caller')

    def test_unknown_cells_cannot_be_asserted(self):
        self.mutate_case('authorization.json', 'scope_check_covers_unknown_role_cells',
                         lambda case: case['caller'].update(scopes=['workspace.read']),
                         'unknown permission')
        self.mutate_case('workspaces.json', 'get_reports_caller_role',
                         lambda case: case['caller'].update(role='viewer'), 'unknown permission')

    def test_credential_kinds_constrain_callers(self):
        file = 'authorization.json'
        self.mutate_case(file, 'machine_token_cannot_invite',
                         lambda case: case['caller']['scopes'].append('workspace.write'), 'cannot carry')
        self.mutate_case(file, 'missing_credential_is_unauthenticated',
                         lambda case: case['caller'].update(role='owner'), 'only user principals')
        self.mutate_case(file, 'missing_credential_is_unauthenticated',
                         lambda case: case['caller'].update(state='lost'), 'unknown credential state')

    def test_matrix_and_labels_follow_the_document(self):
        self.edit_document('| `workspace.delete` | `workspace.write` | allow | deny |',
                           '| `workspace.delete` | `workspace.write` | allow | allow |', 'role matrix')
        self.edit_document('| API-X001 | `identity.self` | Specified |',
                           '| API-X001 | `identity.self` | Held |', 'coverage index label')
        self.edit_document('C6', 'C9', 'does not cite C6')

    def test_limits_and_transitions(self):
        self.mutate_case('account.json', 'feedback_text_above_limit',
                         lambda case: case['limit'].update(value=10), 'within bounds')
        self.mutate_case('audit.json', 'audit_page_above_cap_is_capped',
                         lambda case: case['expect'].update(effective_page=500), 'capped limit')
        self.mutate_case('transitions.json', 'enrollment_consumes_token',
                         lambda case: case['request'].update(event='approve'), 'not in the catalog')
        self.mutate_case('members.json', 'admin_removes_member_without_deleting_account',
                         lambda case: case['expect'].update(state='invited'), 'member cannot become invited')

    def test_rows_must_cover_their_operation(self):
        self.mutate_case('roles.json', 'tier_list_is_retired',
                         lambda case: case['rows'].append('API-X001'), 'does not cover')

    def test_addresses_stay_neutral(self):
        self.mutate_case('members.json', 'member_cannot_invite',
                         lambda case: case['request'].update(emails=['person@real.invalid']), 'use .example')
        self.mutate_case('network_policy.json', 'get_returns_saved_hosts',
                         lambda case: case['given'].update(hosts=['https://real.invalid']), 'reserved .example')

    def test_uncovered_row_fails(self):
        (self.directory / 'vectors/account.json').unlink()
        self.assertInvalid('rows without vectors')


if __name__ == '__main__':
    unittest.main()
