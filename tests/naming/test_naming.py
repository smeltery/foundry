"""Hermetic naming-policy checks: no remote repositories or provider services."""

import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/checks/naming.py"


class NamingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.policy = self.root / "policy.json"
        self.policy.write_text(json.dumps({"version": 1, "rules": [
            {"id": "N001", "pattern": "violetwidget"},
            {"id": "N002", "pattern": r"amber[-_]unit"},
        ]}))
        self.release = self.root / "release"
        self.release.mkdir()
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("GIT_") and key != "F5Y_NAMING_POLICY_JSON"}
        self.env.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull})

    def run_scan(self, *args, policy=True, env=None):
        command = [sys.executable, str(SCRIPT)]
        if policy:
            command += ["--policy", str(self.policy)]
        result = subprocess.run(command + list(args), env=env or self.env,
                                capture_output=True, text=True, timeout=15)
        self.assertEqual(result.stderr, "", result.stderr)
        return result.returncode, json.loads(result.stdout), result.stdout

    def scan_release(self, *args):
        return self.run_scan("--mode", "release", "--release", str(self.release), *args)

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.release), *args],
                              env=self.env, capture_output=True, check=True)

    def repo(self):
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture User")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "commit.gpgsign", "false")

    def commit(self, message="fixture"):
        self.git("add", "--all")
        self.git("commit", "-qm", message)

    def test_clean_text(self):
        (self.release / "readme.txt").write_text("ordinary project\n")
        code, report, _ = self.scan_release()
        self.assertEqual(code, 0)
        self.assertEqual(report["status"], "clean")
        self.assertEqual(report["text_objects"], 1)

    def test_case_insensitive_regex_content_and_redacted_path(self):
        (self.release / "VioletWidget.txt").write_text("safe\nVIOLETWIDGET amber-unit amber_unit\n")
        code, report, output = self.scan_release()
        self.assertEqual(code, 1)
        self.assertEqual(report["match_count"], 4)
        self.assertNotIn("violetwidget", output.lower())
        self.assertNotIn("amber-unit", output.lower())
        self.assertEqual({item["rule_id"] for item in report["findings"]}, {"N001", "N002"})
        self.assertEqual([item["first_line"] for item in report["findings"] if item["field"] == "text"], [2, 2])

    def test_env_policy(self):
        env = dict(self.env, EXTERNAL_POLICY=self.policy.read_text())
        code, report, _ = self.run_scan("--mode", "release", "--release", str(self.release),
                                        "--policy-json-env", "EXTERNAL_POLICY", policy=False, env=env)
        self.assertEqual((code, report["status"]), (0, "clean"))

    def test_missing_and_corrupt_policy_never_echo_input(self):
        code, report, _ = self.run_scan(policy=False)
        self.assertEqual(code, 2)
        self.assertIn("policy_missing_or_oversized", report["errors"])
        for content in ("{violetwidget", '{"version": 1, "rules": []}',
                        '{"version": 1, "rules": [{"id":"N001", "pattern":"["}]}',
                        '{"version": 1, "rules": [{"id":"N001", "pattern":".*"}]}'):
            self.policy.write_text(content)
            code, report, output = self.scan_release()
            self.assertEqual(code, 2)
            self.assertEqual(report["status"], "incomplete")
            self.assertNotIn("violetwidget", output)
        self.policy.unlink()
        self.assertEqual(self.scan_release()[0], 2)

    def test_binary_and_visual_assets_are_not_clearance(self):
        (self.release / "image.png").write_bytes(b"\x89PNG\0data")
        (self.release / "icon.svg").write_text("<svg><text>violetwidget</text></svg>")
        code, report, _ = self.scan_release()
        self.assertEqual(code, 1)
        self.assertTrue(report["uninspected"])
        (self.release / "icon.svg").unlink()
        code, report, _ = self.scan_release()
        self.assertEqual(code, 2)
        self.assertEqual(report["status"], "incomplete")

    def test_index_blobs_not_worktree(self):
        self.repo()
        path = self.release / "item.txt"
        path.write_text("violetwidget")
        self.git("add", "item.txt")
        path.write_text("safe")
        code, report, _ = self.run_scan("--root", str(self.release))
        self.assertEqual(code, 1)
        self.assertEqual(report["match_count"], 1)

    def test_historical_deleted_paths_text_and_commit_messages(self):
        self.repo()
        path = self.release / "VioletWidget.txt"
        path.write_text("amber-unit")
        self.commit("violetwidget")
        path.unlink()
        (self.release / "current.txt").write_text("clean")
        self.commit()
        self.assertEqual(self.run_scan("--root", str(self.release))[0], 0)
        code, report, output = self.run_scan("--mode", "history", "--root", str(self.release))
        self.assertEqual(code, 1)
        self.assertEqual(report["match_count"], 3)
        self.assertNotIn("violetwidget", output.lower())
        self.assertEqual(self.run_scan("--mode", "history", "--root", str(self.release),
                                       "--max-commits", "1")[0], 2)

    def test_history_tag_object_and_shallow_guard(self):
        self.repo()
        (self.release / "item.txt").write_text("safe")
        self.commit()
        self.git("-c", "tag.gpgsign=false", "tag", "-a", "v1", "-m", "violetwidget")
        code, report, _ = self.run_scan("--mode", "history", "--root", str(self.release))
        self.assertEqual(code, 1)
        self.assertTrue(any(item["path"].endswith(":tag") for item in report["findings"]))
        head = self.git("rev-parse", "HEAD").stdout
        (self.release / ".git/shallow").write_bytes(head)
        code, report, _ = self.run_scan("--mode", "history", "--root", str(self.release))
        self.assertEqual(code, 2)
        self.assertIn("shallow_history_incomplete", report["errors"])

    def test_zip_paths_content_traversal_and_links(self):
        archive_path = self.release / "bundle.zip"
        with zipfile.ZipFile(archive_path, "w") as archive:
            archive.writestr("VioletWidget.txt", "amber-unit")
            archive.writestr("../outside.txt", "safe")
            link = zipfile.ZipInfo("link")
            link.create_system = 3
            link.external_attr = (0o120777 << 16)
            archive.writestr(link, "outside.txt")
        code, report, output = self.scan_release()
        self.assertEqual(code, 1)
        self.assertNotIn("violetwidget", output.lower())
        self.assertEqual({item["code"] for item in report["uninspected"]},
                         {"unsafe_archive_path", "archive_link_not_followed"})
        self.assertFalse((self.root / "outside.txt").exists())

    def test_tar_gzip_nested_and_special_entries(self):
        inner = io.BytesIO()
        with zipfile.ZipFile(inner, "w") as archive:
            archive.writestr("note.txt", "violetwidget")
        with tarfile.open(self.release / "bundle.tar.gz", "w:gz") as archive:
            entry = tarfile.TarInfo("nested.zip")
            entry.size = len(inner.getvalue())
            archive.addfile(entry, io.BytesIO(inner.getvalue()))
            link = tarfile.TarInfo("link")
            link.type = tarfile.SYMTYPE
            link.linkname = "/outside"
            archive.addfile(link)
        code, report, _ = self.scan_release()
        self.assertEqual(code, 1)
        self.assertTrue(any(item["path"].endswith("nested.zip!/note.txt") for item in report["findings"]))
        code, report, _ = self.scan_release("--max-depth", "1")
        self.assertEqual(code, 2)
        self.assertIn("archive_depth_limit", {item["code"] for item in report["uninspected"]})

    def test_corrupt_archive_size_and_count_limits(self):
        path = self.release / "bad.zip"
        path.write_bytes(b"invalid")
        self.assertEqual(self.scan_release()[0], 2)
        path.unlink()
        (self.release / "large.txt").write_text("x" * 100)
        self.assertEqual(self.scan_release("--max-bytes", "20")[0], 2)
        self.assertEqual(self.scan_release("--max-total-bytes", "20")[0], 2)
        (self.release / "extra.txt").write_text("x")
        self.assertEqual(self.scan_release("--max-files", "1")[0], 2)

    def test_tar_expansion_limit_including_padding(self):
        with tarfile.open(self.release / "padding.tar.gz", "w:gz") as archive:
            entry = tarfile.TarInfo("empty.txt")
            archive.addfile(entry, io.BytesIO())
        code, report, _ = self.scan_release("--max-total-bytes", "1000")
        self.assertEqual(code, 2)
        self.assertIn("archive_expansion_limit_exceeded", report["errors"])

    def test_filesystem_symlink_not_followed(self):
        outside = self.root / "outside.txt"
        outside.write_text("violetwidget")
        (self.release / "link").symlink_to(outside)
        code, report, _ = self.scan_release()
        self.assertEqual(code, 2)
        self.assertEqual(report["match_count"], 0)
        self.assertEqual(report["uninspected"][0]["code"], "filesystem_link_not_followed")

    def test_submodules_and_large_file_pointers_are_incomplete(self):
        self.repo()
        path = self.release / "pointer.txt"
        path.write_text("version https://git-lfs.github.com/spec/v1\n" + "oid sha256:" + "0" * 64 + "\nsize 12\n")
        self.commit()
        head = self.git("rev-parse", "HEAD").stdout.decode().strip()
        self.git("update-index", "--add", "--cacheinfo", "160000," + head + ",module")
        code, report, _ = self.run_scan("--root", str(self.release))
        self.assertEqual(code, 2)
        self.assertEqual({item["code"] for item in report["uninspected"]},
                         {"submodule_requires_separate_scan", "external_object_not_fetched"})

    def test_corrupt_compression_and_duplicate_policy_ids(self):
        for name, data in (("bad.tar.xz", b"\xfd7zXZ\x00broken"),
                           ("bad.tar.gz", b"\x1f\x8bbroken")):
            path = self.release / name
            path.write_bytes(data)
            code, report, _ = self.scan_release()
            self.assertEqual(code, 2)
            self.assertEqual(report["uninspected"][0]["code"], "archive_unreadable_or_corrupt")
            path.unlink()
        policy = json.loads(self.policy.read_text())
        policy["rules"][1]["id"] = "N001"
        self.policy.write_text(json.dumps(policy))
        self.assertEqual(self.scan_release()[0], 2)

    def test_overlapping_rules_do_not_leak_path(self):
        self.policy.write_text(json.dumps({"version": 1, "rules": [
            {"id": "N001", "pattern": "violet"}, {"id": "N002", "pattern": "letwidget"}]}))
        (self.release / "violetwidget.txt").write_text("safe")
        code, report, output = self.scan_release()
        self.assertEqual(code, 1)
        self.assertNotIn("violet", output)
        self.assertNotIn("letwidget", output)
        self.assertEqual(report["findings"][0]["path"], "[redacted].txt")


if __name__ == "__main__":
    unittest.main()
