"""Release state transitions and fail-closed boundary checks."""

from dataclasses import replace
import importlib.util
from pathlib import Path
import sys
import unittest

MODULE_PATH = Path(__file__).resolve().parents[2] / "scripts/release/planner.py"
SPEC = importlib.util.spec_from_file_location("f5y_release_planner", MODULE_PATH)
planner = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = planner
SPEC.loader.exec_module(planner)

SHA = "a" * 40
OTHER = "b" * 40
ASSETS = (planner.Asset("f5y-linux.tar.gz", "c" * 64),
          planner.Asset("f5y-macos.tar.gz", "d" * 64))
TAG = planner.Tag("v0.1.0", SHA)
BUILD = planner.Build(SHA, ASSETS)


class ReleasePlannerTests(unittest.TestCase):
    def setUp(self):
        self.request = planner.Request(
            True, SHA, planner.CI(SHA, "success", "smeltery/foundry", "ci.yml"),
            "smeltery/foundry", "ci.yml", tuple(asset.name for asset in ASSETS))

    def plan(self, **changes):
        return planner.plan_release(replace(self.request, **changes))

    def reject(self, **changes):
        with self.assertRaises(planner.InvalidSnapshot):
            self.plan(**changes)

    def test_disabled_never_allocates(self):
        result = self.plan(enabled=False)
        self.assertEqual((result.status, result.tag, result.actions), ("disabled", None, ()))

    def test_non_boolean_enablement_rejected(self):
        self.reject(enabled="false")

    def test_non_green_ci_never_allocates(self):
        for status in ("failure", "cancelled", "skipped", "queued", ""):
            with self.subTest(status=status):
                result = self.plan(ci=replace(self.request.ci, conclusion=status))
                self.assertEqual((result.tag, result.actions), (None, ()))

    def test_provenance_mismatches_never_allocate(self):
        for field, value in (("repository", "other/foundry"), ("workflow", "other.yml"),
                             ("event", "pull_request"), ("branch", "feature")):
            with self.subTest(field=field):
                result = self.plan(ci=replace(self.request.ci, **{field: value}))
                self.assertEqual((result.status, result.actions), ("untrusted", ()))

    def test_newer_untested_commit_rejected(self):
        self.reject(candidate_sha=OTHER)

    def test_invalid_commit_formats(self):
        for sha in ("a" * 7, "A" * 40, "g" * 40, SHA + "\n"):
            with self.subTest(sha=sha):
                self.reject(candidate_sha=sha)

    def test_first_allocation_uses_tested_commit_only(self):
        result = self.plan()
        self.assertEqual((result.tag, result.sha, result.actions),
                         ("v0.1.0", SHA, ("create_tag",)))

    def test_numeric_patch_order(self):
        result = self.plan(tags=(planner.Tag("v2.9.9", OTHER),
                                 planner.Tag("v2.10.12", OTHER)))
        self.assertEqual(result.tag, "v2.10.13")

    def test_strict_stable_versions(self):
        for tag in ("v01.2.3", "v1.02.3", "v1.2.03", "v1.2", "1.2.3",
                    "v1.2.3-rc.1", "v1.2.3+build", "v1.2.3\n", "v1.2.-1"):
            with self.subTest(tag=tag):
                self.reject(tags=(planner.Tag(tag, OTHER),))

    def test_retry_after_tag_creation_reuses_tag(self):
        result = self.plan(tags=(TAG, planner.Tag("v0.1.9", OTHER)))
        self.assertEqual((result.tag, result.actions), (TAG.name, ("build_assets",)))

    def test_explicit_recovery_conflict(self):
        self.reject(tags=(planner.Tag(TAG.name, OTHER),), requested_tag=TAG.name)
        self.reject(requested_tag=TAG.name)

    def test_duplicate_and_ambiguous_tags(self):
        self.reject(tags=(TAG, TAG))
        self.reject(tags=(TAG, planner.Tag("v0.1.1", SHA)))

    def test_release_commit_must_match_tag(self):
        self.reject(tags=(TAG,), releases=(planner.Release(TAG.name, OTHER, "draft"),))
        self.reject(releases=(planner.Release(TAG.name, SHA, "draft"),))

    def test_invalid_and_duplicate_releases(self):
        release = planner.Release(TAG.name, SHA, "draft")
        self.reject(tags=(TAG,), releases=(release, release))
        self.reject(tags=(TAG,), releases=(replace(release, state="unknown"),))

    def test_build_commit_must_match(self):
        self.reject(tags=(TAG,), build=replace(BUILD, sha=OTHER))

    def test_incomplete_build_cannot_stage_or_publish(self):
        result = self.plan(tags=(TAG,), build=replace(BUILD, assets=ASSETS[:1]))
        self.assertEqual(result.actions, ("build_assets",))

    def test_inventory_and_digest_validation(self):
        self.reject(expected_assets=())
        self.reject(expected_assets=("../archive",))
        self.reject(expected_assets=("archive", "archive"))
        for assets in ((ASSETS[0], ASSETS[0]), (planner.Asset("archive", "bad"),),
                       (*ASSETS, planner.Asset("extra", "e" * 64))):
            with self.subTest(assets=assets):
                self.reject(tags=(TAG,), build=replace(BUILD, assets=assets))

    def test_complete_build_only_stages(self):
        result = self.plan(tags=(TAG,), build=BUILD)
        self.assertEqual(result.actions, ("create_draft", "upload_assets"))

    def test_partial_draft_retry_only_uploads(self):
        release = planner.Release(TAG.name, SHA, "draft", ASSETS[:1])
        result = self.plan(tags=(TAG,), build=BUILD, releases=(release,))
        self.assertEqual(result.actions, ("upload_assets",))

    def test_complete_verified_draft_has_one_publish_action(self):
        release = planner.Release(TAG.name, SHA, "draft", ASSETS)
        result = self.plan(tags=(TAG,), build=BUILD, releases=(release,))
        self.assertEqual(result.actions, ("publish_release",))

    def test_published_retry_is_noop(self):
        release = planner.Release(TAG.name, SHA, "published", ASSETS)
        result = self.plan(tags=(TAG,), build=BUILD, releases=(release,))
        self.assertEqual((result.status, result.actions), ("complete", ()))

    def test_published_missing_asset_rejected(self):
        release = planner.Release(TAG.name, SHA, "published", ASSETS[:1])
        self.reject(tags=(TAG,), build=BUILD, releases=(release,))

    def test_remote_checksum_conflict_never_overwritten(self):
        for state in ("draft", "published"):
            release = planner.Release(TAG.name, SHA, state,
                                      (replace(ASSETS[0], sha256="e" * 64),))
            self.reject(tags=(TAG,), build=BUILD, releases=(release,))

    def test_planning_does_not_mutate_snapshots(self):
        before = repr(self.request)
        self.assertEqual(self.plan(), self.plan())
        self.assertEqual(repr(self.request), before)

    def test_full_retry_sequence(self):
        self.assertEqual(self.plan().actions, ("create_tag",))
        self.assertEqual(self.plan(tags=(TAG,)).actions, ("build_assets",))
        self.assertEqual(self.plan(tags=(TAG,), build=BUILD).actions,
                         ("create_draft", "upload_assets"))
        release = planner.Release(TAG.name, SHA, "draft", ASSETS[:1])
        self.assertEqual(self.plan(tags=(TAG,), build=BUILD, releases=(release,)).actions,
                         ("upload_assets",))
        release = replace(release, assets=ASSETS)
        self.assertEqual(self.plan(tags=(TAG,), build=BUILD, releases=(release,)).actions,
                         ("publish_release",))
        release = replace(release, state="published")
        self.assertEqual(self.plan(tags=(TAG,), build=BUILD, releases=(release,)).actions, ())

    def test_disabled_cannot_publish_ready_draft(self):
        release = planner.Release(TAG.name, SHA, "draft", ASSETS)
        self.assertEqual(self.plan(enabled=False, tags=(TAG,), build=BUILD,
                                   releases=(release,)).actions, ())

    def test_asset_order_does_not_affect_completion(self):
        release = planner.Release(TAG.name, SHA, "published", tuple(reversed(ASSETS)))
        self.assertEqual(self.plan(tags=(TAG,), build=BUILD,
                                   releases=(release,)).status, "complete")

    def test_unknown_uploaded_asset_rejected(self):
        release = planner.Release(TAG.name, SHA, "draft",
                                  (planner.Asset("extra", "e" * 64),))
        self.reject(tags=(TAG,), build=BUILD, releases=(release,))

    def test_missing_trust_policy_rejected(self):
        self.reject(repository="")
        self.reject(workflow="")


if __name__ == "__main__":
    unittest.main()
