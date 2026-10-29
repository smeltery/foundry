"""Original synthetic Flox-schema fixtures, not generated activation evidence."""

import copy
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[2] / "scripts/ci/flox_verify.py"
SPEC = importlib.util.spec_from_file_location("flox_verify", SOURCE)
verify = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify)


def fixture():
    names = ["rust", "bun", "nodejs", "python", "pre-commit", "cargo-deny",
             "actionlint", "shellcheck", "git", "pkg-config", "cc", "binutils", "toolchain-env"]
    versions = ["1.95.0", "1.3.11", "22.23.1", "3.13.13", "4.5.1", "0.19.9",
                "1.7.12", "0.11.0", "2.54.0", "0.29.2", None, "fixture-only", None]
    systems = ["aarch64-darwin", "x86_64-darwin", "aarch64-linux", "x86_64-linux"]
    origin = "github:example/toolchain/" + "a" * 40
    manifest = {"schema-version": "1.19.0", "minimum-cli-version": "1.19.1",
                "install": {n: {"flake": f"{origin}#{n}"} for n in names},
                "options": {"systems": systems}, "vars": {"RUST_BACKTRACE": "1"}}
    records = []
    for system in systems:
        for name, version in zip(names, versions):
            path = "/nix/store/" + "a" * 32 + f"-{name}-{system}"
            records.append({"install_id": name, "system": system, "package-system": system,
                            "locked-url": origin, "locked-flake-attr-path": f"packages.{system}.{name}",
                            "derivation": path + ".drv", "outputs": {"out": path},
                            "output-names": ["out"], "version": version if name == "toolchain-env" else version or (
                                "21.1.8" if system.endswith("darwin") else "15.3.0")})
    return manifest, {"lockfile-version": 1, "manifest": copy.deepcopy(manifest), "packages": records}


class LockTests(unittest.TestCase):
    def test_complete_synthetic_schema_selects_only_native_outputs(self):
        manifest, lock = fixture()
        for system in manifest["options"]["systems"]:
            with self.subTest(system=system):
                selected = verify.validate_lock(manifest, lock, system)
                self.assertEqual(len(selected), 13)
                self.assertTrue(all(str(paths[0]).endswith(system) for paths in selected.values()))

    def test_incomplete_duplicate_wrong_system_or_wrong_source_fails(self):
        changes = [lambda rows: rows.clear(), lambda rows: rows.pop(),
                   lambda rows: rows.__setitem__(1, rows[0]),
                   lambda rows: rows[0].update({"package-system": "x86_64-linux"}),
                   lambda rows: rows[0].update({"locked-url": "github:example/other/" + "a" * 40}),
                   lambda rows: rows[0].update({"locked-flake-attr-path": "packages.x86_64-linux.rust"}),
                   lambda rows: rows[0].update({"version": "1.95.1"}),
                   lambda rows: rows[0].update({"outputs": {}}),
                   lambda rows: rows[0].update({"outputs-to-install": ["missing"]}),
                   lambda rows: rows[0].update({"broken": True})]
        for change in changes:
            manifest, lock = fixture()
            change(lock["packages"])
            with self.subTest(change=change), self.assertRaises(ValueError):
                verify.validate_lock(manifest, lock, "aarch64-darwin")

    def test_stale_manifest_catalog_and_floating_origin_fail(self):
        for replacement in ({"pkg-path": "git"}, {"flake": "github:example/tools/main#git"}):
            manifest, lock = fixture()
            manifest["install"]["git"] = replacement
            lock["manifest"] = copy.deepcopy(manifest)
            with self.assertRaises(ValueError):
                verify.validate_lock(manifest, lock, "x86_64-linux")
        manifest, lock = fixture()
        lock["manifest"]["vars"]["RUST_BACKTRACE"] = "0"
        with self.assertRaisesRegex(ValueError, "embedded manifest"):
            verify.validate_lock(manifest, lock, "x86_64-linux")

    def test_subdirectory_and_locked_nar_hash_normalization(self):
        manifest, lock = fixture()
        for descriptor in manifest["install"].values():
            descriptor["flake"] = descriptor["flake"].replace("#", "?dir=.flox/toolchain#")
        lock["manifest"] = copy.deepcopy(manifest)
        for record in lock["packages"]:
            record["locked-url"] += "?narHash=sha256-" + "A" * 43 + "%3D&dir=.flox%2Ftoolchain"
        self.assertEqual(len(verify.validate_lock(manifest, lock, "aarch64-darwin")), 13)
        for suffix in ("?dir=elsewhere", "?dir=../escape", "?dir=.flox/toolchain&dir=.flox/toolchain",
                       "?narHash=bad", "?narHash=sha256-AA==", "?unknown=1", "?dir="):
            changed = copy.deepcopy(lock)
            changed["packages"][0]["locked-url"] = "github:example/toolchain/" + "a" * 40 + suffix
            with self.subTest(suffix=suffix), self.assertRaises(ValueError):
                verify.validate_lock(manifest, changed, "aarch64-darwin")
        changed = copy.deepcopy(lock)
        changed["packages"][0]["locked-url"] = "github:example/toolchain/" + "b" * 40 + "?dir=.flox/toolchain"
        with self.assertRaisesRegex(ValueError, "selected origin"):
            verify.validate_lock(manifest, changed, "aarch64-darwin")

    def test_empty_requested_outputs_use_generated_package_selection(self):
        manifest, lock = fixture()
        for record in lock["packages"]:
            record["requested-outputs-to-install"] = []
            record["outputs-to-install"] = ["out"]
        selected = verify.validate_lock(manifest, lock, "aarch64-darwin")
        self.assertEqual(len(selected), 13)
        self.assertEqual(str(selected["rust"][0]),
                         "/nix/store/" + "a" * 32 + "-rust-aarch64-darwin")
        for requested, default in [(["missing"], ["out"]), ([], ["missing"]),
                                   ([], []), (False, ["out"]), ("", ["out"])]:
            changed = copy.deepcopy(lock)
            changed["packages"][0]["requested-outputs-to-install"] = requested
            changed["packages"][0]["outputs-to-install"] = default
            with self.subTest(requested=requested, default=default), self.assertRaises(ValueError):
                verify.validate_lock(manifest, changed, "aarch64-darwin")

    def test_duplicate_json_keys_rejected(self):
        with self.assertRaisesRegex(ValueError, "duplicate"):
            verify.unique_object([("packages", []), ("packages", [])])

    def test_native_system_mapping_and_rejection(self):
        for osname, machine, expected in [("Darwin", "arm64", "aarch64-darwin"),
                                          ("Linux", "x86_64", "x86_64-linux")]:
            with patch.object(verify.platform, "system", return_value=osname), \
                    patch.object(verify.platform, "machine", return_value=machine):
                self.assertEqual(verify.native_system(), expected)
        with patch.object(verify.platform, "system", return_value="Windows"):
            with self.assertRaisesRegex(ValueError, "native system"):
                verify.native_system()


class CommandTests(unittest.TestCase):
    def test_actual_symlink_origin_and_matching_ambient_banner(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected, active, ambient = [root / n for n in ("selected", "active", "ambient")]
            for directory in (selected, active, ambient):
                (directory / "bin").mkdir(parents=True)
            for directory in (selected, ambient):
                tool = directory / "bin" / "git"
                tool.write_text("#!/bin/sh\nprintf 'git version 2.54.0\\n'\n")
                tool.chmod(0o755)
            (active / "bin" / "git").symlink_to(selected / "bin" / "git")
            with patch.dict(os.environ, {"PATH": str(active / "bin")}):
                path = verify.command_origin("git", active, [selected])
                self.assertEqual(verify.output([path, "--version"]), "git version 2.54.0")
            with patch.dict(os.environ, {"PATH": str(ambient / "bin")}):
                with self.assertRaisesRegex(ValueError, "activated environment"):
                    verify.command_origin("git", active, [selected])
            with patch.dict(os.environ, {"PATH": str(active / "bin")}):
                with self.assertRaisesRegex(ValueError, "selected locked output"):
                    verify.command_origin("git", active, [ambient])

    def test_support_script_must_be_the_selected_file_not_identical_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            active, selected, other = [root / name for name in ("active", "selected", "other")]
            relative = Path("share/foundry/toolchain.sh")
            for directory in (active, selected, other):
                (directory / relative).parent.mkdir(parents=True)
            for directory in (selected, other):
                (directory / relative).write_text("export CC=/selected/cc\n")
            with self.assertRaisesRegex(ValueError, "support script"):
                verify.support_origin(active, [selected])
            (active / relative).symlink_to(selected / relative)
            self.assertEqual(verify.support_origin(active, [selected]), str(active / relative))
            with self.assertRaisesRegex(ValueError, "support script"):
                verify.support_origin(active, [other])
            (active / relative).unlink()
            (active / relative).write_bytes((selected / relative).read_bytes())
            with self.assertRaisesRegex(ValueError, "support script"):
                verify.support_origin(active, [selected])

    def test_component_banners_and_exact_versions(self):
        for banner, version in [("rustc 1.95.0 (abc)", "1.95.0"),
                                ("clippy 0.1.95 (abc)", "0.1.95"),
                                ("rustfmt 1.9.0-stable (abc)", "1.9.0-stable"),
                                ("v22.23.1", "22.23.1")]:
            verify.require_version(banner, version, "fixture")
        for banner in ("1.95.01", "1.95.0-nightly", "1.95.0+modified", "11.95.0"):
            with self.assertRaisesRegex(ValueError, "version mismatch"):
                verify.require_version(banner, "1.95.0", "fixture")

    def test_execution_errors_are_failures_without_fallback(self):
        for error in (FileNotFoundError(), subprocess.TimeoutExpired("tool", 60),
                      subprocess.CalledProcessError(1, "tool")):
            with patch.object(verify.subprocess, "run", side_effect=error) as run:
                with self.assertRaisesRegex(ValueError, "command failed"):
                    verify.output(["/selected/tool", "--version"])
                self.assertEqual(run.call_count, 1)

    def test_darwin_linker_only_accepts_known_no_input_diagnostic(self):
        banner = "@(#)PROGRAM:ld PROJECT:ld64-956.6\nld: no object files specified\n"
        error = subprocess.CalledProcessError(1, "ld", output=banner)
        with patch.object(verify.subprocess, "run", side_effect=error):
            self.assertEqual(verify.output(["/selected/ld", "-v"], darwin_linker=True), banner.strip())
            with self.assertRaises(ValueError):
                verify.output(["/selected/ld", "-v"])
        for code, text in [(2, banner), (1, "ld: broken"), (1, "ld: no object files specified")]:
            with patch.object(verify.subprocess, "run", side_effect=subprocess.CalledProcessError(code, "ld", output=text)):
                with self.assertRaises(ValueError):
                    verify.output(["/selected/ld", "-v"], darwin_linker=True)

    def test_smoke_uses_selected_paths_and_failure_stops_link(self):
        commands = {name: f"/selected/{name}" for name in ("cc", "ar", "ranlib")}
        with patch.object(verify, "output", return_value="") as run:
            verify.archive_smoke(commands, True)
        calls = [call.args[0] for call in run.call_args_list]
        self.assertEqual([c[0] for c in calls[:-1]],
                         ["/selected/cc", "/selected/ar", "/selected/ranlib", "/selected/ar", "/selected/cc"])
        self.assertIn("-c", calls[0])
        with patch.object(verify, "output", side_effect=ValueError("compile failed")) as run:
            with self.assertRaises(ValueError):
                verify.archive_smoke(commands, True)
            self.assertEqual(run.call_count, 1)

    def test_full_orchestration_checks_every_command_and_preserves_environment(self):
        selected = {name: [Path("/selected") / name]
                    for name in (*verify.VERSIONS, "cc", "binutils", "toolchain-env")}
        banners = {command: version for command, (_, version) in verify.COMMANDS.items()}
        banners.update({"cc": "21.1.8", "c++": "21.1.8", "ld": "selected linker"})
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / ".flox/env"
            directory.mkdir(parents=True)
            (directory / "manifest.toml").write_text("schema-version = '1.19.0'\n")
            (directory / "manifest.lock").write_text("{}")
            with patch.dict(os.environ, {"FLOX_ENV": "/activated"}), \
                    patch.object(verify, "native_system", return_value="aarch64-darwin"), \
                    patch.object(verify, "validate_lock", return_value=selected), \
                    patch.object(verify, "support_origin", return_value="/activated/share/foundry/toolchain.sh"), \
                    patch.object(verify, "command_origin", side_effect=lambda c, *_: "/activated/bin/" + c) as origins, \
                    patch.object(verify, "output", side_effect=lambda argv, **_: banners[Path(argv[0]).name]) as run, \
                    patch.object(verify, "archive_smoke") as smoke:
                before = dict(os.environ)
                evidence = verify.verify(root, True)
                self.assertEqual(os.environ, before)
            self.assertEqual(set(evidence["commands"]), set(banners) | {"ar", "ranlib"})
            self.assertEqual(origins.call_count, 20)
            self.assertIn(unittest.mock.call(["/activated/bin/actionlint", "-version"]), run.call_args_list)
            smoke.assert_called_once()
            self.assertTrue(evidence["c_link_smoke"])

    def test_missing_lock_fails_before_any_command(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / ".flox/env").mkdir(parents=True)
            (root / ".flox/env/manifest.toml").write_text("schema-version = '1.19.0'\n")
            with patch.object(verify, "output") as run, self.assertRaises(FileNotFoundError):
                verify.verify(root)
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
