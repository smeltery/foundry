"""Offline fixtures exercise payload substitution without executing binaries."""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/release"))
import artifacts
from planner import InvalidSnapshot


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cwd = Path.cwd()
        os.chdir(self.root)
        self.addCleanup(os.chdir, self.cwd)
        self.args = argparse.Namespace(sha="a" * 40, tag="v0.2.3", target="fixture-target", directory="out")
        self.notices = {"LICENSE": b"fixture notice", "notices.json": b"{}"}
        self.paths = {}
        for name in artifacts.BINARIES:
            path = Path("target/fixture-target/release") / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((name + " fixture payload").encode())
            path.chmod(0o755)
            self.paths[name] = path
        self.archive = Path("out/f5y-v0.2.3-fixture-target.tar.gz")

    def command(self, path, flag):
        self.assertEqual(flag, "--version")
        return f"{Path(path).name} 0.2.3"

    def pack(self, command=None):
        artifacts.pack(self.args, self.notices, command or self.command)

    def verify(self):
        return artifacts.verify("out", self.args.tag, self.args.sha, (self.args.target,),
                                (self.archive.name, self.archive.name + ".sha256"), self.notices)

    def rewrite(self, edit):
        with tarfile.open(self.archive, "r:gz") as bundle:
            entries = [(member, bundle.extractfile(member).read()) for member in bundle.getmembers()]
        entries = edit(entries)
        with tarfile.open(self.archive, "w:gz") as bundle:
            for member, content in entries:
                member.size = len(content)
                bundle.addfile(member, io.BytesIO(content))
        self.archive.with_name(self.archive.name + ".sha256").write_text(
            f"{hashlib.sha256(self.archive.read_bytes()).hexdigest()}  {self.archive.name}\n")

    def test_complete_package_is_deterministic_and_binds_both_payloads(self):
        self.pack()
        first = self.archive.read_bytes()
        self.pack()
        self.assertEqual(first, self.archive.read_bytes())
        self.assertEqual(len(self.verify().assets), 2)
        with tarfile.open(self.archive, "r:gz") as bundle:
            manifest = json.load(bundle.extractfile("build.json"))
            self.assertEqual(set(manifest["binaries"]), set(artifacts.BINARIES))
            self.assertEqual(manifest["binaries"]["f5y-toolhost"]["version"], "0.2.3")

    def test_missing_helper_prevents_archive_creation(self):
        self.paths["f5y-toolhost"].unlink()
        with self.assertRaises(InvalidSnapshot):
            self.pack()
        self.assertFalse(self.archive.exists())

    def test_wrong_helper_version_prevents_archive_creation(self):
        def wrong(path, flag):
            return "f5y-toolhost 0.2.2" if Path(path).name == "f5y-toolhost" else self.command(path, flag)
        with self.assertRaises(InvalidSnapshot):
            self.pack(wrong)
        self.assertFalse(self.archive.exists())

    def test_executable_changed_during_version_check_is_rejected(self):
        def changed(path, flag):
            Path(path).write_bytes(b"replacement")
            return self.command(path, flag)
        with self.assertRaises(InvalidSnapshot):
            self.pack(changed)

    def test_rechecks_helper_inventory_even_with_new_outer_checksum(self):
        for mutation in ("missing", "empty", "changed", "mode", "version", "target", "duplicate"):
            with self.subTest(mutation=mutation):
                self.pack()
                def edit(entries):
                    output = []
                    for member, data in entries:
                        if member.name == "f5y-toolhost":
                            if mutation == "missing":
                                continue
                            if mutation == "empty":
                                data = b""
                            if mutation == "changed":
                                data = b"different helper"
                            if mutation == "mode":
                                member.mode = 0o644
                            if mutation == "duplicate":
                                output.append((member, data))
                        if member.name == "build.json" and mutation in {"version", "target"}:
                            value = json.loads(data)
                            if mutation == "version":
                                value["binaries"]["f5y-toolhost"]["version"] = "0.2.2"
                            else:
                                value["target"] = "different-target"
                            data = json.dumps(value, sort_keys=True).encode()
                        output.append((member, data))
                    return output
                self.rewrite(edit)
                with self.assertRaises(InvalidSnapshot):
                    self.verify()

    def test_symlink_and_nonexecutable_helper_fail_before_packaging(self):
        helper = self.paths["f5y-toolhost"]
        helper.chmod(0o644)
        with self.assertRaises(InvalidSnapshot):
            self.pack()
        helper.unlink()
        helper.symlink_to("f5y")
        with self.assertRaises(InvalidSnapshot):
            self.pack()


if __name__ == "__main__":
    unittest.main()
