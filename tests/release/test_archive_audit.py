"""Archive-level notice parity and naming scans over packed, natively compiled executables.

Policies and notice bundles are ephemeral fixtures; the policy token is synthetic and neutral.
"""

import argparse
import io
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/release"))
import archive_audit
import artifacts
import github_snapshot as snapshot
import notices
from planner import InvalidSnapshot
from test_notice_bundle import Fixture

TOKEN = "zephyrquill"
NATIVE = {"Darwin": "MachO64", "Linux": "ELF64"}
PROGRAM = """#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "--version") == 0) { puts(%s); return 0; }
    puts(%s);
    return 0;
}
"""


class ArchiveAuditTests(Fixture):
    def setUp(self):
        super().setUp()
        if shutil.which("cc") is None or platform.system() not in NATIVE:
            if os.environ.get("CI"):
                self.fail("CI must build real native executables for archive audits")
            self.skipTest("Requires a C compiler producing ELF or Mach-O executables")
        cwd = Path.cwd()
        os.chdir(self.base)
        self.addCleanup(os.chdir, cwd)
        self.policy = self.base / "policy/naming.json"
        self.policy.parent.mkdir()
        self.policy.write_text(json.dumps({"version": 1, "rules": [{"id": "N001", "pattern": TOKEN}]}))
        self.build()
        self.bundle = self.generate(output="bundle")[1]
        self.inventory = self.committed_inventory()
        self.args = argparse.Namespace(sha="a" * 40, tag="v0.1.0", target=notices.TARGETS[0], directory="assets")
        self.archive = self.base / "assets" / f"f5y-v0.1.0-{notices.TARGETS[0]}.tar.gz"

    def committed_inventory(self, **changes):
        manifest, files = self.committed(self.bundle)
        tree = {"LICENSE": b"Fixture root license\n", "licenses/cli-template.txt": b"Fixture app notice\n",
                notices.BUNDLE_MANIFEST: manifest, **files, **changes}

        def command(*args, binary=False):
            if args[:2] == ("git", "ls-tree"):
                return "\n".join(sorted(tree))
            return {**tree, "Cargo.lock": self.lock}[args[2].removeprefix("HEAD:")]
        with patch.object(snapshot, "command", side_effect=command):
            return snapshot.notice_inventory()

    def compile(self, target, marker=None):
        """Build real native executables; only the optional marker carries the policy token."""
        for name in artifacts.BINARIES:
            source = self.base / "src" / f"{name}.c"
            source.parent.mkdir(exist_ok=True)
            extra = marker if marker and name == "f5y-toolhost" else "fixture helper output"
            source.write_text(PROGRAM % (json.dumps(f"{name} 0.1.0"), json.dumps(extra)))
            output = self.base / "target" / target / "release" / name
            output.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["cc", "-o", str(output), str(source)], check=True, capture_output=True, timeout=120)

    def pack(self, marker=None, inventory=None, target=notices.TARGETS[0]):
        self.compile(target, marker)
        args = argparse.Namespace(**{**vars(self.args), "target": target})
        artifacts.pack(args, inventory or self.inventory,
                       lambda path, flag: subprocess.check_output([path, flag], text=True, timeout=30).strip())

    def audit(self, committed=None):
        return archive_audit.audit("assets", self.args.tag, self.args.target, committed or self.inventory,
                                   self.lock, ("--policy", str(self.policy)))

    def replace_notices(self, notice_files, **payloads):
        """Rewrite the archive with different notice members and optionally replaced executables."""
        with tarfile.open(self.archive, "r:gz") as bundle:
            kept = {item.name: bundle.extractfile(item).read() for item in bundle.getmembers()
                    if item.name in {*artifacts.BINARIES, "build.json"}}
        with tarfile.open(self.archive, "w:gz") as bundle:
            for name, data in {**kept, **payloads, **notice_files}.items():
                info = tarfile.TarInfo(name)
                info.size, info.mode = len(data), 0o755 if name in artifacts.BINARIES else 0o644
                bundle.addfile(info, io.BytesIO(data))

    def dependency_text(self):
        return next(name for name in self.inventory
                    if name.startswith(notices.BUNDLE_ROOT + "/") and name != notices.BUNDLE_MANIFEST)

    def test_real_native_archive_carries_bundle_and_passes_scans(self):
        self.pack()
        artifacts.verify("assets", self.args.tag, self.args.sha, (self.args.target,),
                         (self.archive.name, self.archive.name + ".sha256"), self.inventory)
        with tarfile.open(self.archive, "r:gz") as bundle:
            names = set(bundle.getnames())
        self.assertIn("notices.json", names)
        self.assertIn(notices.BUNDLE_MANIFEST, names)
        self.assertIn(self.dependency_text(), names)
        expected = NATIVE[platform.system()]
        self.assertEqual(self.audit(), {name: expected for name in artifacts.BINARIES})

    def test_missing_notice_member_fails(self):
        self.pack()
        for name in ("notices.json", notices.BUNDLE_MANIFEST, self.dependency_text(), "LICENSE"):
            reduced = {path: data for path, data in self.inventory.items() if path != name}
            with self.subTest(name=name):
                self.replace_notices(reduced)
                with self.assertRaisesRegex(InvalidSnapshot, "members differ from the release inventory"):
                    self.audit()
                # Omitted from both the archive and its inventory, the manifests still catch it.
                with self.assertRaisesRegex(InvalidSnapshot, "notices"):
                    self.audit(reduced)

    def test_stale_or_modified_notice_fails(self):
        self.pack()
        text = self.dependency_text()
        changed = {**self.inventory, text: b"Fixture text edited after review\n"}
        self.replace_notices(changed)
        with self.assertRaisesRegex(InvalidSnapshot, "differ from the committed bundle"):
            self.audit()
        # A consistent but edited inventory still fails against the hashed bundle manifest.
        summary = json.loads(self.inventory["notices.json"])
        summary["files"][text] = notices.digest(changed[text])
        changed["notices.json"] = json.dumps(summary, sort_keys=True).encode()
        self.replace_notices(changed)
        with self.assertRaisesRegex(InvalidSnapshot, "lack bundle manifest parity"):
            self.audit(changed)
        for key, value in (("lock_sha256", "0" * 64), ("files", {})):
            stale = {**self.inventory, "notices.json": json.dumps(
                {**json.loads(self.inventory["notices.json"]), key: value}, sort_keys=True).encode()}
            with self.subTest(key=key):
                self.replace_notices(stale)
                with self.assertRaisesRegex(InvalidSnapshot, "notices.json does not describe"):
                    self.audit(stale)

    def test_policy_hit_inside_native_executable_fails(self):
        self.pack(marker=f"fixture {TOKEN} output")
        with tarfile.open(self.archive, "r:gz") as bundle:
            self.assertIn(TOKEN.encode(), bundle.extractfile("f5y-toolhost").read())
        with self.assertRaisesRegex(InvalidSnapshot, "Binary scan found policy matches in f5y-toolhost"):
            self.audit()

    def test_policy_hit_inside_text_member_fails(self):
        inventory = self.committed_inventory(LICENSE=f"Fixture {TOKEN} license\n".encode())
        self.pack(inventory=inventory)
        with self.assertRaisesRegex(InvalidSnapshot, "Naming scan found policy matches"):
            self.audit(inventory)

    def test_unsupported_executable_format_is_incomplete(self):
        self.pack()
        self.replace_notices(self.inventory, f5y=b"#!/bin/sh\necho fixture\n")
        with self.assertRaisesRegex(InvalidSnapshot, "Binary scan of f5y was incomplete"):
            self.audit()

    def test_publication_audits_every_target_and_stays_blocked(self):
        for target in snapshot.TARGETS:
            self.pack(target=target)
        with patch.object(snapshot, "notice_inventory", return_value=self.inventory), \
                patch.object(snapshot, "command", return_value=self.lock), \
                patch.dict(os.environ, {"FOUNDRY_NAMING_POLICY": self.policy.read_text()}), \
                patch.object(archive_audit, "audit", wraps=archive_audit.audit) as audit:
            with self.assertRaisesRegex(InvalidSnapshot, "publication remains blocked"):
                snapshot.publication_checks("assets", self.args.tag)
        self.assertEqual([call.args[2] for call in audit.call_args_list], list(snapshot.TARGETS))


if __name__ == "__main__":
    unittest.main()
