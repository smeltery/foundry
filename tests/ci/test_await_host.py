"""Exercise the download host wait with substitute curl and sleep commands."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[2] / "scripts/ci/await-host.sh"
URL = "https://downloads.flox.dev/by-env/stable/"
BACKOFF = [2, 4, 8, 16, 32, 60, 60]

# Fails with FAKE_STATUS for the first FAKE_FAILURES calls, then succeeds.
FAKE_CURL = '''import json, os, pathlib, sys
calls = pathlib.Path(os.environ["CALLS"])
with calls.open("a") as stream:
    stream.write(json.dumps(sys.argv[1:]) + "\\n")
failures = int(os.environ["FAKE_FAILURES"])
if len(calls.read_text().splitlines()) <= failures:
    print("curl: simulated failure", file=sys.stderr)
    sys.exit(int(os.environ.get("FAKE_STATUS", "6")))
'''

FAKE_SLEEP = '''import os, sys
with open(os.environ["SLEEPS"], "a") as stream:
    stream.write(" ".join(sys.argv[1:]) + "\\n")
'''


class AwaitHostTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="await-host-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        bin_directory = self.base / "bin"
        bin_directory.mkdir()
        for name, body in (("curl", FAKE_CURL), ("sleep", FAKE_SLEEP)):
            command = bin_directory / name
            command.write_text(f"#!{sys.executable}\n" + body)
            command.chmod(0o755)
        self.calls = self.base / "calls"
        self.sleeps = self.base / "sleeps"
        self.env = {
            "PATH": f"{bin_directory}:/usr/bin:/bin",
            "CALLS": str(self.calls),
            "SLEEPS": str(self.sleeps),
        }

    def run_wait(self, *arguments, failures, status=6):
        return subprocess.run(
            ["/bin/bash", str(SOURCE), *arguments],
            env={**self.env, "FAKE_FAILURES": str(failures), "FAKE_STATUS": str(status)},
            capture_output=True,
            text=True,
            timeout=15,
        )

    def curl_calls(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()] if self.calls.exists() else []

    def sleep_calls(self):
        return [int(line) for line in self.sleeps.read_text().splitlines()] if self.sleeps.exists() else []

    def test_recovers_after_resolution_failures_with_exponential_backoff(self):
        result = self.run_wait(URL, failures=3)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.curl_calls()), 4)
        self.assertEqual(self.sleep_calls(), [2, 4, 8])
        self.assertIn("Reached downloads.flox.dev on attempt 4", result.stdout)
        self.assertIn("Cannot reach downloads.flox.dev yet (curl exit 6); retrying in 2s", result.stderr)

    def test_every_attempt_is_bounded_and_targets_the_url(self):
        self.run_wait(URL, failures=1)
        for arguments in self.curl_calls():
            self.assertEqual(arguments[-1], URL)
            self.assertIn("--max-time", arguments)
            self.assertIn("--connect-timeout", arguments)

    def test_recovers_on_the_last_attempt_in_the_window(self):
        result = self.run_wait(URL, failures=len(BACKOFF))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.sleep_calls(), BACKOFF)

    def test_persistent_resolution_failure_stops_after_the_window_naming_the_host(self):
        result = self.run_wait(URL, failures=1000)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(self.curl_calls()), len(BACKOFF) + 1)
        self.assertEqual(self.sleep_calls(), BACKOFF)
        self.assertLessEqual(sum(self.sleep_calls()), 240)
        self.assertIn(
            "Could not reach downloads.flox.dev after 8 attempts over 182s of backoff (last curl exit 6)",
            result.stderr,
        )

    def test_other_transient_network_failures_are_retried(self):
        for status in (7, 28, 35):
            with self.subTest(status=status):
                self.calls.unlink(missing_ok=True)
                self.sleeps.unlink(missing_ok=True)
                result = self.run_wait(URL, failures=1, status=status)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.sleep_calls(), [2])

    def test_non_network_failure_stops_without_retrying(self):
        result = self.run_wait(URL, failures=1000, status=60)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(self.curl_calls()), 1)
        self.assertEqual(self.sleep_calls(), [])
        self.assertIn("Cannot reach downloads.flox.dev: curl exited 60", result.stderr)

    def test_rejects_missing_extra_or_insecure_arguments(self):
        for arguments in ((), (URL, URL), ("http://downloads.flox.dev/",)):
            with self.subTest(arguments=arguments):
                result = self.run_wait(*arguments, failures=0)
                self.assertEqual(result.returncode, 2)
                self.assertIn("Usage:", result.stderr)
                self.assertEqual(self.curl_calls(), [])


if __name__ == "__main__":
    unittest.main()
