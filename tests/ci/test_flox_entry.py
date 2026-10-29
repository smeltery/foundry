"""Exercise the locked CI shell entry with an isolated activation substitute."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[2] / "scripts/ci/flox.sh"

FAKE_FLOX = '''import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as stream:
    stream.write(json.dumps(args) + "\\n")
if args == ["--version"]:
    assert os.environ["FLOX_DISABLE_METRICS"] == "true"
    print(os.environ.get("FAKE_VERSION", "1.19.1"))
    sys.exit(0)
assert args[:2] == ["activate", "--dir"], args
assert pathlib.Path(args[2]).resolve() == pathlib.Path.cwd(), args
assert args[3] == "--", args
assert os.environ["FLOX_DISABLE_METRICS"] == "true"
snapshots = list(pathlib.Path(os.environ["TMPDIR"]).iterdir())
assert len(snapshots) == 1, snapshots
assert snapshots[0].read_bytes() == b"locked fixture\\n"
with open(os.environ["EVENTS"], "a") as stream:
    stream.write("activate\\n")
lock = pathlib.Path(".flox/env/manifest.lock")
change = os.environ.get("CHANGE_LOCK")
if change == "replace":
    lock.write_text("different lock\\n")
elif change == "delete":
    lock.unlink()
if os.environ.get("ACTIVATION_STATUS"):
    sys.exit(int(os.environ["ACTIVATION_STATUS"]))
os.environ["FLOX_ENV"] = os.environ["FIXTURE_ENV"]
os.execvpe(args[4], args[4:], os.environ)
'''

VERIFIER = '''import os, sys
with open(os.environ["EVENTS"], "a") as stream:
    stream.write("verify\\n")
sys.exit(int(os.environ.get("VERIFIER_STATUS", "0")))
'''

COMMAND = '''import json, os, pathlib, sys
with open(os.environ["EVENTS"], "a") as stream:
    stream.write("command\\n")
pathlib.Path(os.environ["RESULT"]).write_text(json.dumps({
    "arguments": sys.argv[1:], "cwd": str(pathlib.Path.cwd()),
    "metrics": os.environ["FLOX_DISABLE_METRICS"],
    "support": os.environ["FIXTURE_TOOLCHAIN"],
}))
sys.exit(int(os.environ.get("COMMAND_STATUS", "0")))
'''


class FloxEntryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="flox-entry-")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / "repository with spaces"
        scripts = self.root / "scripts/ci"
        scripts.mkdir(parents=True)
        self.entry = scripts / "flox.sh"
        shutil.copyfile(SOURCE, self.entry)
        (scripts / "flox_verify.py").write_text(VERIFIER)
        self.lock = self.root / ".flox/env/manifest.lock"
        self.lock.parent.mkdir(parents=True)
        self.lock.write_bytes(b"locked fixture\n")
        self.bin = self.base / "bin"
        self.bin.mkdir()
        (self.bin / "python3").symlink_to(sys.executable)
        self.fake = self.bin / "fake-flox"
        self.fake.write_text(f"#!{sys.executable}\n" + FAKE_FLOX)
        self.fake.chmod(0o755)
        # BSD and GNU mktemp choose different defaults without a template.
        allocator = self.bin / "mktemp"
        allocator.write_text(f"#!{sys.executable}\n" + '''import os, sys, tempfile
assert len(sys.argv) == 1
descriptor, path = tempfile.mkstemp(dir=os.environ["TMPDIR"])
os.close(descriptor)
print(path)
''')
        allocator.chmod(0o755)
        self.command = self.root / "command.py"
        self.command.write_text(COMMAND)
        self.scratch = self.base / "snapshots"
        self.scratch.mkdir()
        self.events = self.base / "events"
        self.calls = self.base / "calls"
        self.result = self.base / "result.json"
        self.environment = self.base / "activated environment"
        self.support = self.environment / "share/foundry/toolchain.sh"
        self.support.parent.mkdir(parents=True)
        self.support.write_text(
            'printf "support\\n" >> "$EVENTS"\n'
            'export FIXTURE_TOOLCHAIN="selected fixture"\n'
        )
        self.env = {
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "TMPDIR": str(self.scratch),
            "FOUNDRY_FLOX": str(self.fake),
            "FLOX_DISABLE_METRICS": "false",
            "EVENTS": str(self.events),
            "CALLS": str(self.calls),
            "RESULT": str(self.result),
            "FIXTURE_ENV": str(self.environment),
        }

    def run_entry(self, *arguments, no_command=False, **settings):
        command = [] if no_command else [sys.executable, str(self.command), *arguments]
        result = subprocess.run(
            ["/bin/bash", str(self.entry), *command],
            cwd=self.base,
            env={**self.env, **settings},
            capture_output=True,
            text=True,
            timeout=15,
        )
        self.assertEqual(list(self.scratch.iterdir()), [], "snapshot was not removed")
        return result

    def assert_stopped(self, events):
        observed = self.events.read_text().splitlines() if self.events.exists() else []
        self.assertEqual(observed, events)
        self.assertFalse(self.result.exists(), "user command executed")

    def assert_version_only(self):
        self.assertEqual(self.calls.read_text().splitlines(), ['["--version"]'])

    def test_wrong_version_stops_before_activation(self):
        result = self.run_entry(FAKE_VERSION="1.19.0")
        self.assertEqual(result.returncode, 1)
        self.assertIn("require Flox 1.19.1", result.stderr)
        self.assert_version_only()
        self.assert_stopped([])

    def test_missing_lock_stops_before_activation(self):
        self.lock.unlink()
        result = self.run_entry()
        self.assertEqual(result.returncode, 1)
        self.assertIn("lock is missing", result.stderr)
        self.assert_version_only()
        self.assert_stopped([])

    def test_empty_lock_stops_before_activation(self):
        self.lock.write_bytes(b"")
        result = self.run_entry()
        self.assertEqual(result.returncode, 1)
        self.assertIn("lock is missing", result.stderr)
        self.assert_version_only()
        self.assert_stopped([])

    def test_missing_command_stops_before_activation(self):
        result = self.run_entry(no_command=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("Usage:", result.stderr)
        self.assert_version_only()
        self.assert_stopped([])

    def test_activation_failure_preserves_status_and_cleans_snapshot(self):
        result = self.run_entry(ACTIVATION_STATUS="23")
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assert_stopped(["activate"])
        self.assertEqual(self.lock.read_bytes(), b"locked fixture\n")

    def test_verifier_failure_prevents_command_and_cleans_snapshot(self):
        result = self.run_entry(VERIFIER_STATUS="31")
        self.assertEqual(result.returncode, 31, result.stderr)
        self.assert_stopped(["activate", "verify"])

    def test_success_preserves_arguments_working_directory_and_order(self):
        arguments = ["two words", "", "$(touch forbidden)", "line\nbreak", "--flag"]
        result = self.run_entry(*arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.events.read_text().splitlines(), ["activate", "verify", "support", "command"])
        self.assertEqual(json.loads(self.result.read_text()), {
            "arguments": arguments, "cwd": str(self.root.resolve()), "metrics": "true",
            "support": "selected fixture",
        })
        self.assertFalse((self.root / "forbidden").exists())
        self.assertEqual(self.lock.read_bytes(), b"locked fixture\n")

    def test_command_failure_status_is_preserved(self):
        result = self.run_entry(COMMAND_STATUS="47")
        self.assertEqual(result.returncode, 47, result.stderr)
        self.assertEqual(self.events.read_text().splitlines(), ["activate", "verify", "support", "command"])
        self.assertTrue(self.result.exists())

    def test_lock_replacement_fails_after_successful_command(self):
        result = self.run_entry(CHANGE_LOCK="replace")
        self.assertEqual(result.returncode, 1)
        self.assertIn("changed the lock", result.stderr)
        self.assertTrue(self.result.exists())
        self.assertEqual(self.lock.read_text(), "different lock\n")

    def test_lock_deletion_fails_after_successful_command(self):
        result = self.run_entry(CHANGE_LOCK="delete")
        self.assertEqual(result.returncode, 1)
        self.assertIn("changed the lock", result.stderr)
        self.assertTrue(self.result.exists())
        self.assertFalse(self.lock.exists())

    def test_lock_change_also_fails_when_activation_fails(self):
        result = self.run_entry(CHANGE_LOCK="replace", ACTIVATION_STATUS="23")
        self.assertEqual(result.returncode, 1)
        self.assertIn("changed the lock", result.stderr)
        self.assert_stopped(["activate"])

    def test_defined_global_flags_reject_before_support_or_command(self):
        for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"):
            for value in ("", "fixture override"):
                with self.subTest(name=name, value=value):
                    self.events.unlink(missing_ok=True)
                    result = self.run_entry(**{name: value})
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn("selected target compiler flags", result.stderr)
                    self.assert_stopped(["activate", "verify"])

    def test_missing_support_script_prevents_command(self):
        self.support.unlink()
        result = self.run_entry()
        self.assertNotEqual(result.returncode, 0)
        self.assert_stopped(["activate", "verify"])

    def test_nonempty_compiler_overrides_reject_before_support_or_command(self):
        for name in (
            "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        ):
            with self.subTest(name=name):
                self.events.unlink(missing_ok=True)
                result = self.run_entry(**{name: "fixture override"})
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"reject compiler overrides: {name}", result.stderr)
                self.assert_stopped(["activate", "verify"])


if __name__ == "__main__":
    unittest.main()
