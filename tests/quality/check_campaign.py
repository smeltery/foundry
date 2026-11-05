import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import socket
import struct
import sys
import tempfile
import threading
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tests/quality/campaign.json"
sys.path.insert(0, str(ROOT / "scripts/quality"))
SPEC = importlib.util.spec_from_file_location("campaign", ROOT / "scripts/quality/campaign.py")
CAMPAIGN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CAMPAIGN)
ENDPOINT = CAMPAIGN.endpoint

REPLY_KEYS = {"task", "seed", "repeat", "request_sha256", "status", "text", "tool_calls",
              "input_tokens", "output_tokens"}


class CampaignTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def manifest_copy(self, **changes):
        """A copy of the pinned manifest and its fixtures, with top-level fields replaced."""
        shutil.copytree(ROOT / "tests/quality/tasks", self.root / "tasks", dirs_exist_ok=True)
        manifest = json.loads(MANIFEST.read_text())
        manifest.update(changes)
        path = self.root / "campaign.json"
        path.write_text(json.dumps(manifest))
        return path

    def records(self, files):
        return [json.loads(line) for line in files["outcomes.jsonl"].decode().splitlines()]

    def test_two_runs_write_byte_identical_files(self):
        first = CAMPAIGN.run(MANIFEST, self.root / "first")
        second = CAMPAIGN.run(MANIFEST, self.root / "second")
        for name in ("manifest.json", "outcomes.jsonl"):
            self.assertEqual((self.root / "first" / name).read_bytes(),
                             (self.root / "second" / name).read_bytes())
            self.assertEqual(first[name], second[name])

    def test_every_task_runs_per_seed_and_repeat_in_order(self):
        records = self.records(CAMPAIGN.run(MANIFEST, self.root / "out"))
        manifest = json.loads(MANIFEST.read_text())
        self.assertEqual(len(records), len(manifest["tasks"]) * len(manifest["seeds"]) * manifest["repeats"])
        keys = [(record["task"], record["seed"], record["repeat"]) for record in records]
        self.assertEqual(keys[:4], [("summarize-notes", 7, 0), ("summarize-notes", 7, 1),
                                    ("summarize-notes", 1031, 0), ("summarize-notes", 1031, 1)])
        # A repeat sends the same request; another seed sends a different one.
        self.assertEqual(records[0]["request_sha256"], records[1]["request_sha256"])
        self.assertNotEqual(records[0]["request_sha256"], records[2]["request_sha256"])
        self.assertEqual({record["status"] for record in records}, {"replied", "tool_requested"})

    def test_records_carry_no_grading_fields(self):
        files = CAMPAIGN.run(MANIFEST, self.root / "out")
        for record in self.records(files):
            self.assertEqual(set(record), REPLY_KEYS)
        self.assertEqual(json.loads(files["manifest.json"])["grading"], "none")

    def test_fake_and_local_providers_record_the_same_outcomes(self):
        local = CAMPAIGN.run(self.manifest_copy(provider="local"), self.root / "local")
        fake = CAMPAIGN.run(self.manifest_copy(provider="fake"), self.root / "fake")
        self.assertEqual(local["outcomes.jsonl"], fake["outcomes.jsonl"])
        self.assertEqual(json.loads(fake["manifest.json"])["provider"], "fake")

    def test_connections_stay_on_loopback(self):
        addresses = []
        original = socket.create_connection

        def recording(address, *args, **kwargs):
            addresses.append(address[0])
            return original(address, *args, **kwargs)

        with mock.patch("socket.create_connection", recording):
            CAMPAIGN.run(MANIFEST, self.root / "out")
        self.assertEqual(set(addresses), {"127.0.0.1"})
        with mock.patch("socket.create_connection", side_effect=AssertionError("network used")):
            CAMPAIGN.run(self.manifest_copy(provider="fake"), self.root / "fake")

    def test_client_refuses_hosts_that_are_not_literal_loopback(self):
        for host in ("192.0.2.10", "10.0.0.1", "localhost", "stub.example"):
            with self.assertRaises(ValueError):
                ENDPOINT.LoopbackClient(host, 8080)

    def test_endpoint_failure_is_recorded_not_graded(self):
        manifest, tasks = CAMPAIGN.load_manifest(MANIFEST)
        records = CAMPAIGN.records_for(manifest, tasks, failure_status=503)
        self.assertEqual(records[0]["status"], "endpoint_failure")
        self.assertEqual(records[0]["failure"], {"category": "http", "status": 503})
        self.assertNotIn("text", records[0])

    def test_reset_connection_is_a_connect_failure(self):
        # A bound but idle socket is refused on Linux yet left to time out on macOS, so the
        # listener here accepts and resets instead. It holds its port for the whole test.
        def reset(listener):
            connection, _ = listener.accept()
            connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
            connection.close()

        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            thread = threading.Thread(target=reset, args=(listener,))
            thread.start()
            client = ENDPOINT.LoopbackClient("127.0.0.1", listener.getsockname()[1])
            with self.assertRaises(ENDPOINT.EndpointFailure) as caught:
                client.complete({"messages": []})
            thread.join()
        self.assertEqual(caught.exception.category, "connect")

    def test_malformed_replies_are_protocol_failures(self):
        for reply in ({}, {"choices": []}, {"choices": [{"message": {"content": ""}}]},
                      {"choices": [{"message": {"tool_calls": [{"id": "a"}]}}]}):
            with self.assertRaises(ENDPOINT.EndpointFailure) as caught:
                ENDPOINT.parse_reply(reply)
            self.assertEqual(caught.exception.category, "protocol")

    def test_pinned_fixture_digest_is_enforced(self):
        path = self.manifest_copy()
        (self.root / "tasks/rename-symbol.json").write_text('{"id": "rename-symbol"}')
        with self.assertRaisesRegex(CAMPAIGN.ManifestError, "digest mismatch"):
            CAMPAIGN.load_manifest(path)

    def test_invalid_manifest_fields_are_refused(self):
        for changes in ({"effort": "extreme"}, {"repeats": 0}, {"repeats": True}, {"seeds": []},
                        {"seeds": [1, 1]}, {"provider": "hosted-a"}, {"provider_id": "Bad Label"},
                        {"tasks": [{"path": "../campaign.json", "sha256": "0"}]}, {"schema": 2},
                        {"grading": "none"}):
            with self.subTest(changes=changes), self.assertRaises(CAMPAIGN.ManifestError):
                CAMPAIGN.load_manifest(self.manifest_copy(**changes))

    def test_unknown_fixture_keys_are_refused(self):
        task = json.loads((ROOT / "tests/quality/tasks/read-file-tool.json").read_text())
        for name, change in (("task", {"score": 1}),
                             ("tool", {"tools": [dict(task["tools"][0], strict=True)]})):
            data = json.dumps(dict(task, **change)).encode()
            path = self.manifest_copy(tasks=[{"path": "tasks/changed.json",
                                              "sha256": hashlib.sha256(data).hexdigest()}])
            (self.root / "tasks/changed.json").write_bytes(data)
            with self.subTest(name), self.assertRaises(CAMPAIGN.ManifestError):
                CAMPAIGN.load_manifest(path)

    def test_verify_reports_identical_digests(self):
        with mock.patch("builtins.print") as printed:
            CAMPAIGN.verify(MANIFEST)
        lines = [call.args[0] for call in printed.call_args_list]
        self.assertEqual(len(lines), 2)
        self.assertTrue(all("identical across two runs" in line for line in lines))


if __name__ == "__main__":
    unittest.main()
