"""Offline checks for the neutral provider vectors and their validator."""
import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest

import validate_provider_vectors as vectors

HERE = Path(__file__).resolve().parent


def succeed_with(**fields):
    """Return a change that turns an error expectation into a success with the fields."""
    def change(case):
        case['expect'].pop('error')
        case['expect'].update(result='ok', **fields)
    return change


class ProviderVectorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name) / 'providers'
        shutil.copytree(HERE, self.directory, ignore=shutil.ignore_patterns('__pycache__'))
        self.catalog = vectors.read_json(self.directory / 'catalog.json')

    def load(self, name):
        return vectors.read_json(self.directory / 'vectors' / name)

    def save(self, name, document):
        (self.directory / 'vectors' / name).write_text(json.dumps(document))

    def assertInvalid(self, pattern):
        with self.assertRaisesRegex(vectors.InvalidVectors, pattern):
            vectors.validate(vectors.ROOT, self.directory)

    def mutate_case(self, name, case_name, change, pattern):
        """Apply one change to a copy of the named case, expect rejection, then restore."""
        original = self.load(name)
        document = copy.deepcopy(original)
        case = next(case for case in document['cases'] if case['name'] == case_name)
        change(case)
        self.save(name, document)
        try:
            self.assertInvalid(pattern)
        finally:
            self.save(name, original)

    def test_repository_vectors_are_valid(self):
        self.assertGreater(vectors.validate(), 0)

    def test_every_audit_row_has_a_section_and_operations(self):
        rows = self.catalog['rows']
        self.assertEqual(len(rows), 23)
        for row, entry in rows.items():
            with self.subTest(row=row):
                self.assertTrue(entry['operations'])
                self.assertRegex(row, vectors.ROW)

    def test_case_structure_and_error_classes(self):
        self.mutate_case('cli.json', 'empty_stdin_token_is_invalid',
                         lambda case: case['expect'].update(error='teapot'), 'unknown error class')
        self.mutate_case('cli.json', 'empty_stdin_token_is_invalid',
                         lambda case: case['expect'].pop('error'), 'exactly one error class')
        self.mutate_case('cli.json', 'empty_stdin_token_is_invalid',
                         lambda case: case['rows'].append('AG-12'), 'does not cover')

    def test_implemented_cases_need_existing_tests(self):
        self.mutate_case('models.json', 'selection_identity_covers_provider_model_and_effort',
                         lambda case: case.pop('evidence'), 'need evidence')
        missing = ['crates/contracts/tests/adapter_selection.rs::no_such_test']
        self.mutate_case('models.json', 'selection_identity_covers_provider_model_and_effort',
                         lambda case: case.update(evidence=missing), 'evidence test does not exist')

    def test_write_only_values_are_never_expected(self):
        self.mutate_case('cli.json', 'token_stdin_is_stored_without_echo',
                         lambda case: case['expect'].update(echo='fixture-token-canary'), 'write-only')
        self.mutate_case('oauth_logins.json', 'respond_accepts_current_prompt_without_echo',
                         lambda case: case['expect'].update(prompt={'answer': 'fixture-code-canary'}), 'write-only')
        self.mutate_case('endpoints.json', 'timeout_failure_hides_url_and_key',
                         lambda case: case['expect'].update(message='http://127.0.0.1:8080/v1 timed out'),
                         'write-only')

    def test_state_machines_follow_the_catalog(self):
        self.mutate_case('credential_slots.json', 'credential_refusal_marks_auth_failed',
                         lambda case: case['expect'].update(state='reconnect_required'), 'not in the catalog')
        self.mutate_case('oauth_logins.json', 'cancel_terminal_login_leaves_it_unchanged',
                         lambda case: case['expect'].update(state='cancelled'), 'not in the catalog')
        self.mutate_case('oauth_logins.json', 'cancel_pending_login',
                         lambda case: case['given'].update(state='purged'), 'purged cannot become cancelled')
        self.mutate_case('subscription.json', 'refreshable_credential_connects',
                         lambda case: case['expect'].update(state='revoked'), 'unknown credential state')

    def test_limits_follow_the_catalog(self):
        self.mutate_case('budgets.json', 'deadline_below_one_minute_is_invalid',
                         lambda case: case['limit'].update(value=60000), 'within bounds')
        self.mutate_case('budgets.json', 'zero_deadline_means_none',
                         lambda case: case['expect'].update(result='error', error='invalid_request'), 'within bounds')
        self.mutate_case('oauth_logins.json', 'oversize_answer_is_invalid',
                         lambda case: case['expect'].update(error='conflict'), 'must fail with invalid_request')

    def test_money_and_reservation_arithmetic(self):
        self.mutate_case('budgets.json', 'decimal_money_is_exact',
                         lambda case: case['given'].update(ceiling=0.3), 'decimal string')
        self.mutate_case('budgets.json', 'reservation_within_ceiling_succeeds',
                         lambda case: case['given'].update(committed='0.5000000001'), 'decimal string')
        self.mutate_case('budgets.json', 'concurrent_reservations_have_one_winner_when_one_fits',
                         lambda case: case['expect'].update(winners=2), '1 of 2 reservations fit')
        self.mutate_case('budgets.json', 'reservation_over_ceiling_is_budget_exhausted',
                         lambda case: case['request'].update(amounts=['0.50']), '1 of 1 reservations fit')
        self.mutate_case('budgets.json', 'settle_above_reservation_records_overrun',
                         lambda case: case['expect'].update(dispatch_allowed=True), 'settlement arithmetic')
        self.mutate_case('budgets.json', 'settle_below_reservation_releases_rest',
                         lambda case: case['expect'].update(released='0.20'), 'settlement arithmetic')
        self.mutate_case('budgets.json', 'unverifiable_review_end_needs_review',
                         lambda case: case['expect'].update(outcome='success'), 'never reports success')

    def test_auto_resolution_and_effort(self):
        self.mutate_case('models.json', 'auto_resolves_once_and_is_recorded',
                         lambda case: case['expect'].pop('resolution'), 'recorded concrete model')
        self.mutate_case('models.json', 'omitted_model_means_auto',
                         lambda case: case['expect']['resolution'].update(resolved_model='auto'),
                         'recorded concrete model')
        self.mutate_case('models.json', 'effort_the_model_does_not_offer_is_invalid',
                         succeed_with(model='fixture-small'), 'not offered')
        self.mutate_case('models.json', 'offered_effort_is_recorded',
                         lambda case: case['given']['models'][0].update(available=False), 'not listed and available')
        self.mutate_case('models.json', 'list_orders_models_and_flags_availability',
                         lambda case: case['given']['models'][0]['efforts'].append('turbo'), 'outside the vocabulary')

    def test_endpoint_network_classes(self):
        self.mutate_case('endpoints.json', 'lan_http_needs_its_own_opt_in',
                         lambda case: case['request'].update(opt_in=['lan']), 'must be lan')
        self.mutate_case('endpoints.json', 'public_https_endpoint_needs_no_opt_in',
                         lambda case: case['request'].update(base_url='http://models.example/v1'),
                         'must be insecure_transport')
        self.mutate_case('endpoints.json', 'tailnet_with_opt_in_is_admitted',
                         lambda case: case['given'].update(resolved=['100.100.10.5', '192.168.1.2']),
                         'must be endpoint_not_permitted')
        self.mutate_case('endpoints.json', 'link_local_metadata_address_is_always_refused',
                         succeed_with(network_class='link_local'), 'must be endpoint_not_permitted')
        self.mutate_case('endpoints.json', 'timeout_failure_hides_url_and_key',
                         lambda case: case['expect'].update(category='disk'), 'known category')

    def test_addresses_and_identifiers_stay_neutral(self):
        self.mutate_case('endpoints.json', 'public_https_endpoint_needs_no_opt_in',
                         lambda case: case['given'].update(resolved=['8.8.8.8']), 'global address')
        self.mutate_case('endpoints.json', 'public_https_endpoint_needs_no_opt_in',
                         lambda case: case['request'].update(base_url='https://models.invalid/v1'), '.example')
        self.mutate_case('credential_slots.json', 'configure_stores_key_and_returns_only_names',
                         lambda case: case['request'].update(provider_id='Vendor_Name'), 'not neutral')
        self.mutate_case('accounts.json', 'usage_reports_windows',
                         lambda case: case['request'].update(path='/home/user'), 'under /workspace/')

    def test_document_drift_and_uncovered_rows_fail(self):
        document = self.load('consumption.json')
        self.save('consumption.json', dict(document, cases=[c for c in document['cases']
                                                           if 'API-E2F0CEEFEB828' not in c['rows']]))
        self.assertInvalid(r"rows without vectors: \['API-E2F0CEEFEB828'\]")
        self.save('consumption.json', document)
        catalog = copy.deepcopy(self.catalog)
        catalog['rows']['CLI-032']['section'] = 'missing-section'
        (self.directory / 'catalog.json').write_text(json.dumps(catalog))
        self.assertInvalid('CLI-032: no section')
        catalog['rows']['CLI-032']['section'] = 'local-and-custom-endpoints'
        catalog['rows']['VENDOR-1'] = {'section': 'local-and-custom-endpoints', 'operations': ['endpoint.admit']}
        (self.directory / 'catalog.json').write_text(json.dumps(catalog))
        self.assertInvalid('row ID is not neutral')


if __name__ == '__main__':
    unittest.main()
