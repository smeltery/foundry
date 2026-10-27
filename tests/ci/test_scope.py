"""Check routing boundaries that could accidentally omit required CI coverage."""

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[2] / "scripts/checks/ci_scope.py"
SPEC = importlib.util.spec_from_file_location("ci_scope", SOURCE)
scope = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scope)


class ScopeTests(unittest.TestCase):
    def test_main_manual_empty_and_unknown_paths_keep_full_coverage(self):
        for event, paths in [("push", ["docs/user/cli.md"]), ("workflow_dispatch", []),
                             ("pull_request", []), ("pull_request", ["future/component.md"]),
                             ("pull_request", ["docs/../Cargo.toml"])]:
            with self.subTest(event=event, paths=paths):
                self.assertEqual(scope.select_groups(event, paths), scope.GROUPS)

    def test_shared_roots_and_nested_manifests_cover_all_consumers(self):
        for path in ["Cargo.lock", "README.md", ".github/workflows/ci.yml", ".flox/env/manifest.toml",
                     "crates/contracts/src/lib.rs", "crates/mcp/Cargo.toml", "scripts/checks/all.py",
                     "tests/ci/test_scope.py", "vendor/netwatch/src/lib.rs"]:
            with self.subTest(path=path):
                self.assertEqual(scope.select_groups("pull_request", [path]), scope.GROUPS)

    def test_docs_are_scoped_but_code_markdown_is_not_assumed_documentation(self):
        self.assertEqual(scope.select_groups("pull_request", ["docs/user/cli.md"]), ("docs", "hygiene"))
        groups = scope.select_groups("pull_request", ["crates/runtime/src/prompt.md"])
        self.assertTrue({"rust", "integration", "mcp-sdk"}.issubset(groups))

    def test_mixed_components_keep_integration_on_all_platforms(self):
        groups = scope.select_groups("pull_request", ["packages/adapter/src/protocol.ts", "apps/cli/src/main.rs"])
        self.assertTrue({"adapter", "rust", "integration", "mcp-sdk", "docs", "hygiene"}.issubset(groups))
        jobs = scope.matrix(groups)["include"]
        self.assertEqual({job["runner"] for job in jobs if job["group"] == "integration"},
                         {"ubuntu-24.04", "ubuntu-24.04-arm", "macos-15"})
        self.assertEqual(len(scope.matrix(scope.GROUPS)["include"]), 10)

    def test_real_git_rename_and_deletion_retain_original_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-C", directory, *args], text=True).strip()
            git("init", "-q")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "docs").mkdir()
            (root / "Cargo.toml").write_text("fixture\n")
            (root / "docs/removed.md").write_text("removed\n")
            git("add", ".")
            git("-c", "commit.gpgsign=false", "commit", "-qm", "fixture")
            base = git("rev-parse", "HEAD")
            git("mv", "Cargo.toml", "docs/moved.md")
            git("rm", "docs/removed.md")
            git("-c", "commit.gpgsign=false", "commit", "-qm", "move fixture")
            head = git("rev-parse", "HEAD")
            paths = scope.changed_paths(base, head, cwd=directory)
            self.assertEqual(set(paths), {"Cargo.toml", "docs/moved.md", "docs/removed.md"})
            self.assertEqual(scope.select_groups("pull_request", paths), scope.GROUPS)

    def test_invalid_comparison_never_invokes_git(self):
        with patch.object(scope.subprocess, "check_output") as command:
            with self.assertRaises(ValueError):
                scope.changed_paths("--help", "a" * 40)
            command.assert_not_called()


if __name__ == "__main__":
    unittest.main()
