"""Offline verification of the CI installer's integrity and version boundaries."""

import argparse
from contextlib import ExitStack, redirect_stderr
import hashlib
import importlib.util
import io
import runpy
import sys
import tarfile
from types import ModuleType
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

SOURCE = Path(__file__).resolve().parents[2] / "scripts/ci/bootstrap.py"
SPEC = importlib.util.spec_from_file_location("ci_bootstrap", SOURCE)
bootstrap = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bootstrap)


def archive(entries):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as output:
        for name, kind in entries:
            entry = tarfile.TarInfo(name)
            entry.type = kind
            entry.size = 4 if kind == tarfile.REGTYPE else 0
            entry.linkname = "elsewhere" if kind == tarfile.SYMTYPE else ""
            output.addfile(entry, io.BytesIO(b"tool") if entry.size else None)
    result = data.getvalue()
    return result, hashlib.sha256(result).hexdigest()


class IntegrityTests(unittest.TestCase):
    def test_only_named_regular_member_is_read(self):
        data, digest = archive([("../../outside", tarfile.REGTYPE), ("bin/tool", tarfile.REGTYPE)])
        self.assertEqual(bootstrap.executable_payload(data, digest, "bin/tool"), b"tool")

    def test_modified_archive_is_rejected_before_tar_parsing(self):
        data, digest = archive([("tool", tarfile.REGTYPE)])
        with self.assertRaisesRegex(ValueError, "SHA256"):
            bootstrap.executable_payload(data + b"altered", digest, "tool")

    def test_missing_duplicate_and_symlink_executables_rejected(self):
        for entries in ([], [("tool", tarfile.REGTYPE)] * 2, [("tool", tarfile.SYMTYPE)]):
            with self.subTest(entries=entries):
                data, digest = archive(entries)
                with self.assertRaisesRegex(ValueError, "regular executable"):
                    bootstrap.executable_payload(data, digest, "tool")

    def test_archive_size_bound(self):
        data, digest = archive([("tool", tarfile.REGTYPE)])
        with patch.object(bootstrap, "MAX_ARCHIVE", len(data) - 1):
            with self.assertRaisesRegex(ValueError, "size"):
                bootstrap.executable_payload(data, digest, "tool")

    def test_version_is_exact_token(self):
        with patch.object(bootstrap, "output", return_value="rustc 1.95.0 (commit)"):
            bootstrap.require_version(("rustc", "--version"), "1.95.0")
        for value in ("rustc 1.95.01", "rustc 1.95.0-nightly", "rustc 1.99.0"):
            with self.subTest(value=value), patch.object(bootstrap, "output", return_value=value):
                with self.assertRaises(ValueError):
                    bootstrap.require_version(("rustc", "--version"), "1.95.0")

    def test_hygiene_rejects_unsupported_platform_before_download(self):
        with patch.object(bootstrap, "require_version"), patch.object(bootstrap, "output"), \
                patch.object(bootstrap.platform, "system", return_value="Darwin"), \
                patch.object(bootstrap.platform, "machine", return_value="arm64"), \
                patch.object(bootstrap, "install") as install:
            with self.assertRaisesRegex(ValueError, "Linux x86_64"):
                bootstrap.bootstrap("hygiene")
            install.assert_not_called()


class FloxBootstrapTests(unittest.TestCase):
    def run_flox(self, group, failure=None):
        verifier = ModuleType("flox_verify")
        verifier.verify = Mock(side_effect=failure)
        forbidden = {
            name: Mock(side_effect=AssertionError(f"legacy {name} called"))
            for name in ("bootstrap", "install", "prepend_path", "output", "require_version")
        }
        parse_args = argparse.ArgumentParser.parse_args
        stderr = io.StringIO()
        with ExitStack() as stack:
            def parse_and_guard(parser, *args, **kwargs):
                # runpy creates fresh globals; guard them after definitions, before dispatch.
                namespace = sys._getframe(1).f_globals
                stack.enter_context(patch.dict(namespace, forbidden))
                return parse_args(parser, *args, **kwargs)

            stack.enter_context(patch.object(argparse.ArgumentParser, "parse_args", parse_and_guard))
            stack.enter_context(patch.object(sys, "argv", [str(SOURCE), "--group", group, "--flox"]))
            stack.enter_context(patch.dict(sys.modules, {"flox_verify": verifier}))
            download = stack.enter_context(patch(
                "urllib.request.urlopen", side_effect=AssertionError("download called")))
            process = stack.enter_context(patch(
                "subprocess.check_output", side_effect=AssertionError("legacy process called")))
            stack.enter_context(redirect_stderr(stderr))
            try:
                runpy.run_path(str(SOURCE), run_name="__main__")
                status = 0
            except SystemExit as error:
                status = error.code
        verifier.verify.assert_called_once_with()
        for mock in [*forbidden.values(), download, process]:
            mock.assert_not_called()
        return status, stderr.getvalue()

    def test_flox_verifies_without_legacy_setup_for_every_group(self):
        for group in bootstrap.GROUPS:
            with self.subTest(group=group):
                status, stderr = self.run_flox(group)
                self.assertEqual(status, 0)
                self.assertEqual(stderr, "")

    def test_flox_verification_failure_exits_without_installer_fallback(self):
        for failure in (ValueError("fixture version mismatch"), OSError("fixture tool missing")):
            for group in ("dependencies", "hygiene"):
                with self.subTest(group=group, failure=type(failure).__name__):
                    status, stderr = self.run_flox(group, failure)
                    self.assertEqual(status, 1)
                    self.assertIn(f"CI bootstrap failed: {failure}", stderr)


if __name__ == "__main__":
    unittest.main()
