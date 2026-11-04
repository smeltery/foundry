"""Offline fixtures for elected-license notice bundles and release notice parity."""

import argparse
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/release"))
import github_snapshot as snapshot
import notices
from planner import InvalidSnapshot

SOURCE = "registry+https://example.invalid/index"
TEXTS = {"LICENSE-MIT": b"Fixture permission text one\n", "LICENSE-APACHE": b"Fixture permission text two\n",
         "LICENSE-BOOST": b"Fixture permission text three\n", "NOTICE": b"Fixture attribution line\r\n"}
COVERS = {"LICENSE-MIT": "MIT", "LICENSE-APACHE": "Apache-2.0", "LICENSE-BOOST": "BSL-1.0", "NOTICE": "notice"}


class Fixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)

    def build(self, expression="MIT OR Apache-2.0", files=("LICENSE-MIT", "LICENSE-APACHE", "NOTICE")):
        """Collect fixture evidence for one registry dependency of one workspace package."""
        root = self.base / "registry/src/test-index/widget-1.0.0"
        root.mkdir(parents=True)
        archive = self.base / "registry/cache/test-index/widget-1.0.0.crate"
        archive.parent.mkdir(parents=True)
        contents = {"Cargo.toml": b'[package]\nname="widget"\nversion="1.0.0"\n', **{name: TEXTS[name] for name in files}}
        with tarfile.open(archive, "w:gz") as bundle:
            for path, data in contents.items():
                (root / path).write_bytes(data)
                member = tarfile.TarInfo("widget-1.0.0/" + path)
                member.size = len(data)
                bundle.addfile(member, io.BytesIO(data))
        self.checksum = notices.digest(archive.read_bytes())
        self.lock = ('version=4\n[[package]]\nname="app"\nversion="0.1.0"\n\n[[package]]\nname="widget"\n'
                     f'version="1.0.0"\nsource="{SOURCE}"\nchecksum="{self.checksum}"\n').encode()
        widget = {"name": "widget", "version": "1.0.0", "source": SOURCE, "license": expression,
                  "license_file": None, "manifest_path": str(root / "Cargo.toml")}
        app = {"name": "app", "version": "0.1.0", "source": None, "license": None,
               "license_file": None, "manifest_path": str(self.base / "app/Cargo.toml")}
        tree = "0app v0.1.0|default\n1widget v1.0.0|default\n"
        data = {target: ({"packages": [app, widget]}, tree, tree) for target in notices.TARGETS}
        self.evidence, texts = notices.inventory(self.lock, data, "fixture-cargo")
        self.evidence_dir = self.base / "evidence"
        for path, content in {**texts, "manifest.json": notices.encoded(self.evidence)}.items():
            (self.evidence_dir / path).parent.mkdir(parents=True, exist_ok=True)
            (self.evidence_dir / path).write_bytes(content)
        self.files = files

    def review(self, elected="MIT", covers=None, **changes):
        covers = covers if covers is not None else {name: COVERS[name] for name in self.files}
        package = {"name": "widget", "version": "1.0.0", "source": SOURCE, "package_sha256": self.checksum,
                   "expression": self.evidence["packages"][1]["expression"], "elected": elected,
                   "files": [{"source_path": name, "sha256": notices.digest(TEXTS[name]), "covers": value}
                             for name, value in covers.items()]}
        record = {"schema": 1, "lock_sha256": self.evidence["lock_sha256"],
                  "graph_sha256": self.evidence["graph_sha256"], "packages": [package]}
        return json.dumps({**record, **changes}).encode()

    def generate(self, review=None, output="out"):
        output = self.base / output
        return notices.generate(self.lock, self.evidence_dir, review or self.review(), output), output

    def committed(self, output):
        files = {path.relative_to(output).as_posix(): path.read_bytes()
                 for path in (output / notices.BUNDLE_ROOT).rglob("*") if path.is_file()}
        return files.pop(notices.BUNDLE_MANIFEST), files


class BundleTests(Fixture):
    def test_mit_election_writes_only_elected_and_mandatory_texts(self):
        self.build()
        bundle, output = self.generate()
        prefix = f"licenses/rust/widget-1.0.0-{self.checksum[:12]}/"
        manifest, files = self.committed(output)
        self.assertEqual(set(files), {prefix + "LICENSE-MIT", prefix + "NOTICE"})
        self.assertEqual(files[prefix + "NOTICE"], TEXTS["NOTICE"])
        self.assertEqual([(item["name"], item["elected"]) for item in bundle["packages"]], [("widget", "MIT")])
        self.assertEqual(notices.verify_bundle(manifest, files, self.lock), bundle)
        with self.assertRaises(notices.InventoryError):
            self.generate()  # An existing bundle is never merged with new output.

    def test_legacy_slash_expression_is_a_list_of_alternatives(self):
        self.build("MIT/Apache-2.0")
        self.assertIn("license_expression_invalid", self.evidence["packages"][1]["findings"])
        self.assertEqual(self.generate()[0]["packages"][0]["elected"], "MIT")
        with self.assertRaises(notices.InventoryError):
            self.generate(self.review("MIT AND Apache-2.0"), "conjunction")

    def test_package_without_mit_option_requires_an_offered_election(self):
        self.build("Apache-2.0 OR BSL-1.0", ("LICENSE-APACHE", "LICENSE-BOOST", "NOTICE"))
        for elected in ("MIT", "MIT OR Apache-2.0", "Apache-2.0 AND BSL-1.0"):
            with self.subTest(elected=elected), self.assertRaises(notices.InventoryError):
                self.generate(self.review(elected), elected)
        _, files = self.committed(self.generate(self.review("Apache-2.0"))[1])
        self.assertEqual(sorted(Path(path).name for path in files), ["LICENSE-APACHE", "NOTICE"])

    def test_compound_election_needs_every_elected_text(self):
        self.assertEqual(notices.elected_terms("(MIT OR Apache-2.0) AND Unicode-3.0", "Unicode-3.0 AND MIT")[0],
                         {"MIT", "Unicode-3.0"})
        with self.assertRaises(notices.InventoryError):
            notices.require_texts({"MIT", "Unicode-3.0"}, [{"covers": "MIT"}])

    def test_missing_or_misclassified_text_is_rejected(self):
        self.build("MIT OR Apache-2.0", ("LICENSE-APACHE", "NOTICE"))
        with self.assertRaises(notices.InventoryError):
            self.generate()
        reviews = (self.review("Apache-2.0", {"NOTICE": "notice"}),
                   self.review("Apache-2.0", {"LICENSE-APACHE": "BSL-1.0", "NOTICE": "notice"}),
                   self.review("Apache-2.0", {"LICENSE-APACHE": "notice", "NOTICE": "notice"}))
        for index, review in enumerate(reviews):
            with self.subTest(index=index), self.assertRaises(notices.InventoryError):
                self.generate(review, f"case-{index}")
        tampered = json.loads(self.review("Apache-2.0"))
        tampered["packages"][0]["files"][0]["sha256"] = "0" * 64
        with self.assertRaises(notices.InventoryError):
            self.generate(json.dumps(tampered).encode(), "tampered")

    def test_drift_between_lock_review_and_manifest_is_rejected(self):
        self.build()
        _, output = self.generate()
        manifest, files = self.committed(output)
        with self.assertRaises(notices.InventoryError):
            notices.verify_bundle(manifest, files, self.lock + b"\n")
        reviews = (self.review(lock_sha256="0" * 64), self.review(packages=[]),
                   self.review(graph_sha256="0" * 64))
        for index, review in enumerate(reviews):
            with self.subTest(index=index), self.assertRaises(notices.InventoryError):
                self.generate(review, f"drift-{index}")
        value = json.loads(manifest)
        for edit in (lambda item: item.update(packages=[]),
                     lambda item: item["packages"][0].update(package_sha256="0" * 64),
                     lambda item: item["graphs"].pop(notices.TARGETS[0]),
                     lambda item: item["packages"][0].update(elected="Apache-2.0")):
            changed = json.loads(manifest)
            edit(changed)
            with self.assertRaises(notices.InventoryError):
                notices.verify_bundle(notices.encoded(changed), files, self.lock)
        with self.assertRaises(notices.InventoryError):
            notices.verify_bundle(json.dumps(value).encode(), files, self.lock)

    def test_unexpected_extra_missing_or_changed_notice_is_rejected(self):
        self.build()
        manifest, files = self.committed(self.generate()[1])
        first = sorted(files)[0]
        extra = f"licenses/rust/widget-1.0.0-{self.checksum[:12]}/LICENSE-APACHE"
        for changed in ({**files, extra: TEXTS["LICENSE-APACHE"]}, {**files, "licenses/rust/other/NOTICE": b"x"},
                        {path: data for path, data in files.items() if path != first}, {**files, first: b"changed"}):
            with self.assertRaises(notices.InventoryError):
                notices.verify_bundle(manifest, changed, self.lock)


class ReleaseParityTests(Fixture):
    def tree(self, output, **changes):
        manifest, files = self.committed(output)
        tree = {"LICENSE": b"root", "licenses/cli-template.txt": b"app", notices.BUNDLE_MANIFEST: manifest,
                **files, "Cargo.lock": self.lock, **changes}

        def command(*args, binary=False):
            if args[:2] == ("git", "rev-parse"):
                return "a" * 40
            if args[:2] == ("git", "ls-tree"):
                return "\n".join(path for path in sorted(tree) if path != "Cargo.lock" and tree[path] is not None)
            return tree[args[2].removeprefix("HEAD:")]
        return patch.object(snapshot, "command", side_effect=command)

    def test_notice_inventory_requires_exact_bundle_parity(self):
        self.build()
        _, output = self.generate()
        with self.tree(output):
            inventory = snapshot.notice_inventory()
        summary = json.loads(inventory["notices.json"])
        self.assertNotIn("dependency_coverage", summary)
        self.assertEqual(summary["lock_sha256"], notices.digest(self.lock))
        for changes in ({"licenses/rust/extra/NOTICE": b"unexpected"}, {notices.BUNDLE_MANIFEST: None},
                        {"Cargo.lock": self.lock + b"\n"}):
            with self.subTest(changes=list(changes)), self.tree(output, **changes), self.assertRaises(InvalidSnapshot):
                snapshot.notice_inventory()

    def test_pack_requires_current_target_graph_and_publication_stays_blocked(self):
        self.build()
        _, output = self.generate()
        args = argparse.Namespace(sha="a" * 40, tag="v0.1.0", target=notices.TARGETS[0])
        graph = self.evidence["graphs"][notices.TARGETS[0]]
        for current in ({"runtime": [], "build_context": []}, graph):
            with self.subTest(matches=current is graph), self.tree(output), \
                    patch.dict("os.environ", {"FOUNDRY_WORKFLOW_SHA": args.sha}), \
                    patch.object(snapshot.notices, "target_graph", return_value=current), \
                    patch.object(snapshot.artifacts, "pack") as packer:
                if current is graph:
                    snapshot.pack(args)
                    self.assertIn(notices.BUNDLE_MANIFEST, packer.call_args.args[1])
                else:
                    with self.assertRaises(InvalidSnapshot):
                        snapshot.pack(args)
                    packer.assert_not_called()
        with self.tree(output), patch.object(snapshot.archive_audit, "audit"), \
                patch.object(snapshot.archive_audit, "naming_scan"), \
                self.assertRaisesRegex(InvalidSnapshot, "publication remains blocked"):
            snapshot.publication_checks("artifacts", "v0.1.0")


if __name__ == "__main__":
    unittest.main()
