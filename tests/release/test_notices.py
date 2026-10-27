"""Offline fixtures for package provenance and unapproved notice inventories."""

import copy
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "f5y_notices", Path(__file__).resolve().parents[2] / "scripts/release/notices.py")
notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notices)


class NoticeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "registry/src/test-index/widget-1.0.0"
        self.root.mkdir(parents=True)
        self.package = {"name": "widget", "version": "1.0.0", "source": "registry+https://example.invalid/index",
                        "license": "MIT OR Apache-2.0", "license_file": None,
                        "manifest_path": str(self.root / "Cargo.toml")}
        self.write_package({"Cargo.toml": b'[package]\nname="widget"\nversion="1.0.0"\n',
                            "LICENSE-MIT": b"Exact notice\nCopyright fixture owner\n",
                            "NOTICE": b"Preserve these attribution bytes\r\n"})

    def write_package(self, files):
        self.archive = self.root.parents[2] / "cache/test-index/widget-1.0.0.crate"
        self.archive.parent.mkdir(parents=True, exist_ok=True)
        with tarfile.open(self.archive, "w:gz") as bundle:
            for path, data in files.items():
                local = self.root / path
                local.parent.mkdir(parents=True, exist_ok=True)
                local.write_bytes(data)
                member = tarfile.TarInfo("widget-1.0.0/" + path)
                member.size = len(data)
                bundle.addfile(member, io.BytesIO(data))
        checksum = notices.digest(self.archive.read_bytes())
        self.lock = ('version=4\n[[package]]\nname="widget"\nversion="1.0.0"\n'
                     'source="registry+https://example.invalid/index"\n'
                     f'checksum="{checksum}"\n').encode()

    def collect(self):
        metadata = {"packages": [self.package]}
        tree = "0widget v1.0.0|default\n"
        return notices.inventory(self.lock, {"test-target": (metadata, tree, tree)}, "fixture-cargo")

    def test_exact_text_and_repeatable_inventory(self):
        manifest, texts = self.collect()
        self.assertEqual((manifest, texts), self.collect())
        self.assertIn(b"Preserve these attribution bytes\r\n", texts.values())
        self.assertEqual(manifest["status"], "review_required")
        self.assertIn("copyright_and_notice_obligations_review_required", manifest["packages"][0]["findings"])
        notices.verify_inventory(manifest, texts, self.lock, manifest["graphs"])

    def test_duplicate_lock_package_rejected(self):
        with self.assertRaises(notices.InventoryError):
            notices.lock_packages(self.lock + self.lock[self.lock.index(b"[[package]]"):])

    def test_duplicate_metadata_identity_rejected(self):
        with self.assertRaises(notices.InventoryError):
            notices.graph_packages("0widget v1.0.0|\n", {"packages": [self.package, self.package]})

    def test_repeated_graph_node_is_one_package(self):
        selected, rows = notices.graph_packages("0widget v1.0.0|default\n1widget v1.0.0|default (*)\n",
                                               {"packages": [self.package]})
        self.assertEqual(len(selected), 1)
        self.assertEqual(rows[1]["features"], ["default"])

    def test_multiple_release_roots_preserve_helper_only_dependencies(self):
        helper = {**self.package, "name": "helper"}
        dependency = {**self.package, "name": "helper-dependency"}
        selected, rows = notices.graph_packages(
            "0widget v1.0.0|default\n\n0helper v1.0.0|\n1helper-dependency v1.0.0|extra\n",
            {"packages": [self.package, helper, dependency]})
        self.assertEqual({key[0] for key in selected}, {"widget", "helper", "helper-dependency"})
        self.assertEqual([row["depth"] for row in rows], [0, 0, 1])

    def test_missing_notice_is_gap(self):
        self.write_package({"Cargo.toml": b"fixture manifest"})
        manifest, texts = self.collect()
        self.assertEqual(texts, {})
        self.assertIn("notice_text_missing", manifest["packages"][0]["findings"])

    def test_missing_archive_is_gap(self):
        self.archive.unlink()
        manifest, _ = self.collect()
        self.assertIn("source_archive_missing", manifest["packages"][0]["findings"])

    def test_missing_declared_file_is_gap(self):
        self.package["license_file"] = "missing.txt"
        manifest, _ = self.collect()
        self.assertIn("declared_license_file_missing", manifest["packages"][0]["findings"])

    def test_altered_source_notice_rejected(self):
        (self.root / "NOTICE").write_bytes(b"altered")
        with self.assertRaises(notices.InventoryError):
            self.collect()

    def test_altered_source_archive_rejected(self):
        self.archive.write_bytes(self.archive.read_bytes() + b"extra")
        with self.assertRaises(notices.InventoryError):
            self.collect()

    def test_altered_manifest_rejected(self):
        (self.root / "Cargo.toml").write_bytes(b"changed")
        with self.assertRaises(notices.InventoryError):
            self.collect()

    def test_altered_collected_text_or_missing_text_rejected(self):
        manifest, texts = self.collect()
        first = next(iter(texts))
        for changed in ({**texts, first: b"changed"}, {key: value for key, value in texts.items() if key != first}):
            with self.assertRaises(notices.InventoryError):
                notices.verify_inventory(manifest, changed, self.lock, manifest["graphs"])

    def test_different_lock_and_target_graph_rejected(self):
        manifest, texts = self.collect()
        with self.assertRaises(notices.InventoryError):
            notices.verify_inventory(manifest, texts, self.lock + b"\n", manifest["graphs"])
        with self.assertRaises(notices.InventoryError):
            notices.verify_inventory(manifest, texts, self.lock, {})

    def test_duplicate_inventory_entry_rejected(self):
        manifest, texts = self.collect()
        duplicate = copy.deepcopy(manifest)
        duplicate["packages"].append(duplicate["packages"][0])
        with self.assertRaises(notices.InventoryError):
            notices.verify_inventory(duplicate, texts, self.lock, manifest["graphs"])

    def test_expression_syntax_and_unknown_terms(self):
        self.assertEqual(notices.license_findings("(MIT OR Apache-2.0) AND BSD-3-Clause"), [])
        for expression in ("MIT/Apache-2.0", "MIT AND", "(MIT", "MIT MIT", "MIT WITH"):
            with self.subTest(expression=expression):
                self.assertIn("license_expression_invalid", notices.license_findings(expression))
        self.assertTrue(notices.license_findings("LicenseRef-custom"))
        self.assertTrue(notices.license_findings("Apache-2.0 WITH LLVM-exception"))
        self.assertTrue(notices.license_findings(None))

    def test_missing_package_or_wrong_source_digest_rejected(self):
        manifest, texts = self.collect()
        for packages in ([], [{**manifest["packages"][0], "package_sha256": "0" * 64}]):
            with self.assertRaises(notices.InventoryError):
                notices.verify_inventory({**manifest, "packages": packages}, texts, self.lock, manifest["graphs"])

    def test_repository_output_refused_before_cargo_runs(self):
        with patch.object(notices.subprocess, "check_output") as cargo, self.assertRaises(notices.InventoryError):
            notices.collect(Path(self.temp.name), Path(self.temp.name) / "licenses/rust")
        cargo.assert_not_called()


if __name__ == "__main__":
    unittest.main()
