"""Decide the aggregate required check from job records, without network access."""

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/ci"))
import required_gate  # noqa: E402

EMPTY = '{"include":[]}'
PLANNED = '{"include":[{"system":"x86_64-darwin"}]}'


def job(name, conclusion, runner="GitHub Actions 1"):
    return {"name": name, "conclusion": conclusion, "runner_name": runner}


LINUX = [job("check (rust, ubuntu-24.04)", "success"), job("check (quick, ubuntu-24.04)", "success")]


class RequiredGateTests(unittest.TestCase):
    def decide(self, check, activation, jobs, verified, plan="success", activation_plan=PLANNED):
        return required_gate.decide(plan, check, activation, activation_plan, jobs, verified)[0]

    def test_all_success_passes_without_job_records(self):
        self.assertTrue(self.decide("success", "success", [], False))
        self.assertTrue(self.decide("success", "skipped", [], False, activation_plan=EMPTY))

    def test_plan_failure_and_skipped_planned_activation_fail(self):
        self.assertFalse(self.decide("success", "success", [], True, plan="failure"))
        self.assertFalse(self.decide("success", "skipped", [], True))

    def test_unassigned_macos_needs_local_verification(self):
        jobs = LINUX + [job("check (integration, macos-15)", "cancelled", runner=None),
                        job("required", "in_progress")]
        self.assertFalse(self.decide("cancelled", "success", jobs, False))
        self.assertTrue(self.decide("cancelled", "success", jobs, True))

    def test_unassigned_darwin_activation_is_excused_the_same_way(self):
        jobs = LINUX + [job("activation (x86_64-darwin)", "cancelled", runner="")]
        self.assertTrue(self.decide("success", "cancelled", jobs, True))

    def test_macos_that_ran_and_failed_or_was_cancelled_still_blocks(self):
        for conclusion in ("failure", "cancelled", "timed_out"):
            jobs = LINUX + [job("check (integration, macos-15)", conclusion)]
            self.assertFalse(self.decide("failure", "success", jobs, True), conclusion)

    def test_any_linux_failure_blocks_even_with_local_verification(self):
        jobs = [job("check (rust, ubuntu-24.04)", "failure"),
                job("check (integration, macos-15)", "cancelled", runner=None)]
        self.assertFalse(self.decide("failure", "success", jobs, True))

    def test_unknown_failure_without_records_blocks(self):
        self.assertFalse(self.decide("failure", "success", [], True))


if __name__ == "__main__":
    unittest.main()
