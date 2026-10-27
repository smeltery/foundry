"""Offline regressions for refreshed CI, tag and publication authority."""

import argparse
from dataclasses import replace
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/release"))
import github_snapshot as snapshot
from planner import Asset, Build, InvalidSnapshot, Release


class GateTests(unittest.TestCase):
    def setUp(self):
        self.sha = "a" * 40
        self.repository = "smeltery/foundry"
        self.args = argparse.Namespace(sha=self.sha, tag="v0.1.0", run_id=42, attempt=1,
                                       directory="fixture-assets")
        self.run = {"head_sha": self.sha, "conclusion": "success", "event": "push", "head_branch": "main",
                    "head_repository": {"full_name": self.repository},
                    "repository": {"full_name": self.repository}, "workflow_id": 7,
                    "run_attempt": 1, "status": "completed"}
        self.current = dict(self.run)
        self.tags = [{"name": self.args.tag}]
        self.tag_sha = self.sha
        self.reads = []
        self.start(patch.dict(os.environ, {"GITHUB_REPOSITORY": self.repository,
                                          "FOUNDRY_WORKFLOW_SHA": self.sha}))
        self.start(patch.object(snapshot, "api", side_effect=self.api))
        self.commands = self.start(patch.object(snapshot, "command", return_value=self.sha))

    def start(self, patcher):
        value = patcher.start()
        self.addCleanup(patcher.stop)
        return value

    def api(self, path, payload=None, pages=False):
        self.assertIsNone(payload, "snapshot must never mutate remote state")
        self.reads.append(path)
        prefix = f"repos/{self.repository}"
        values = {f"{prefix}/actions/workflows/ci.yml": {"id": 7},
                  f"{prefix}/actions/runs/42/attempts/1": self.run,
                  f"{prefix}/actions/runs/42": self.current,
                  f"{prefix}/commits/main": {"sha": self.sha},
                  f"{prefix}/compare/{self.sha}...{self.sha}": {"status": "identical"},
                  f"{prefix}/tags?per_page=100": self.tags,
                  f"{prefix}/commits/v0.1.0": {"sha": self.tag_sha}}
        return values[path]

    def test_current_green_exact_attempt_and_tag_accept(self):
        request, plan = snapshot.snapshot(self.args)
        self.assertEqual(plan.status, "build")
        self.assertEqual(request.requested_tag, self.args.tag)
        self.assertIn(f"repos/{self.repository}/actions/runs/42", self.reads)

    def test_historical_green_cannot_override_current_attempt(self):
        for change in ({"run_attempt": 2}, {"run_attempt": 2, "conclusion": "failure"},
                       {"status": "in_progress", "conclusion": None}, {"conclusion": "failure"},
                       {"head_sha": "b" * 40}, {"workflow_id": 8}, {"event": "pull_request"}):
            with self.subTest(change=change):
                self.current = dict(self.run, **change)
                with self.assertRaises(InvalidSnapshot):
                    snapshot.snapshot(self.args)

    def test_tag_absence_or_retarget_never_becomes_recovery_reservation(self):
        self.tags = []
        with self.assertRaises(InvalidSnapshot):
            snapshot.snapshot(self.args)
        self.tags = [{"name": self.args.tag}]
        self.tag_sha = "b" * 40
        with self.assertRaises(InvalidSnapshot):
            snapshot.snapshot(self.args)

    def test_initial_reservation_without_requested_tag_remains_possible(self):
        self.tags = []
        self.args.tag = ""
        self.assertEqual(snapshot.snapshot(self.args)[1].status, "reserve")

    def test_workflow_revision_is_required_before_remote_reads_or_pack(self):
        for revision in ("", "main", "b" * 40, self.sha.upper()):
            with self.subTest(revision=revision), patch.dict(os.environ, {"FOUNDRY_WORKFLOW_SHA": revision}):
                self.reads.clear()
                with self.assertRaises(InvalidSnapshot):
                    snapshot.snapshot(self.args)
                self.assertEqual(self.reads, [])
                with patch.object(snapshot.artifacts, "pack") as packer, self.assertRaises(InvalidSnapshot):
                    snapshot.pack(self.args)
                packer.assert_not_called()

    def setup_publisher(self, releases):
        self.asset = Asset("fixture", "c" * 64)
        self.build = Build(self.sha, (self.asset,))
        self.start(patch.object(snapshot, "expected_assets", return_value=("fixture",)))
        self.start(patch.object(snapshot, "verified_build", return_value=self.build))
        # Only offline state-machine fixtures substitute the intentional blocker.
        self.start(patch.object(snapshot, "publication_checks"))
        return self.start(patch.object(snapshot, "remote_release", side_effect=releases))

    def draft(self, **changes):
        return replace(Release(self.args.tag, self.sha, "draft", (Asset("fixture", "c" * 64),)), **changes)

    def test_final_remote_payload_change_prevents_patch(self):
        self.setup_publisher([(9, self.draft()), (9, self.draft(assets=(Asset("fixture", "d" * 64),)))])
        with self.assertRaises(InvalidSnapshot):
            snapshot.publish(self.args)
        self.assertFalse(any("PATCH" in call.args for call in self.commands.call_args_list))

    def test_final_missing_assets_or_release_prevents_patch(self):
        for final in ((9, self.draft(assets=())), (None, None)):
            with self.subTest(final=final):
                with patch.object(snapshot, "expected_assets", return_value=("fixture",)), \
                        patch.object(snapshot, "verified_build", return_value=Build(self.sha, (Asset("fixture", "c" * 64),))), \
                        patch.object(snapshot, "publication_checks"), \
                        patch.object(snapshot, "remote_release", side_effect=[(9, self.draft()), final]):
                    with self.assertRaises(InvalidSnapshot):
                        snapshot.publish(self.args)
                self.assertFalse(any("PATCH" in call.args for call in self.commands.call_args_list))

    def test_final_tag_or_ci_change_prevents_patch(self):
        for change in ("tag", "ci", "workflow"):
            with self.subTest(change=change):
                self.tags = [{"name": self.args.tag}]
                self.current = dict(self.run)
                os.environ["FOUNDRY_WORKFLOW_SHA"] = self.sha
                def first_release(*args):
                    if change == "tag":
                        self.tags = []
                    elif change == "ci":
                        self.current["run_attempt"] = 2
                    else:
                        os.environ["FOUNDRY_WORKFLOW_SHA"] = "b" * 40
                    return 9, self.draft()
                with patch.object(snapshot, "expected_assets", return_value=("fixture",)), \
                        patch.object(snapshot, "verified_build", return_value=Build(self.sha, (Asset("fixture", "c" * 64),))), \
                        patch.object(snapshot, "publication_checks"), \
                        patch.object(snapshot, "remote_release", side_effect=first_release):
                    with self.assertRaises(InvalidSnapshot):
                        snapshot.publish(self.args)
                self.assertFalse(any("PATCH" in call.args for call in self.commands.call_args_list))

    def test_refreshed_complete_release_returns_without_patch(self):
        self.setup_publisher([(9, self.draft()), (9, self.draft(state="published"))])
        snapshot.publish(self.args)
        self.assertFalse(any("PATCH" in call.args for call in self.commands.call_args_list))

    def test_patch_uses_fresh_release_identity_and_reads_back(self):
        remote = self.setup_publisher([(9, self.draft()), (10, self.draft()),
                                       (10, self.draft(state="published"))])
        snapshot.publish(self.args)
        patches = [call for call in self.commands.call_args_list if "PATCH" in call.args]
        self.assertEqual(len(patches), 1)
        self.assertIn(f"repos/{self.repository}/releases/10", patches[0].args)
        self.assertEqual(remote.call_count, 3)

    def test_workflows_supply_actual_context_revision(self):
        root = Path(__file__).resolve().parents[2]
        for name in ("auto-release", "release"):
            workflow = (root / f".github/workflows/{name}.yml").read_text()
            self.assertIn("FOUNDRY_WORKFLOW_SHA: ${{ github.workflow_sha }}", workflow)
            self.assertIn('= "$CANDIDATE_SHA"', workflow)
            self.assertNotIn("FOUNDRY_WORKFLOW_SHA: ${{ inputs.sha }}", workflow)


if __name__ == "__main__":
    unittest.main()
