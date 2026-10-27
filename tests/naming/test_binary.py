"""Safe synthetic executable headers and fail-closed coverage contracts."""

import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/checks/binary.py"


def elf(payload=b""):
    data = bytearray(120)
    ident = b"\x7fELF\x02\x01\x01" + b"\0" * 9
    struct.pack_into("<16sHHIQQQIHHHHHH", data, 0, ident, 2, 62, 1, 0, 64, 0, 0, 64, 56, 1, 0, 0, 0)
    struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 0, 0, 0, len(data) + len(payload), len(data) + len(payload), 4096)
    return data + payload


def macho(payload=b""):
    data = bytearray(104)
    struct.pack_into("<IiiIIIII", data, 0, 0xFEEDFACF, 0x100000C, 0, 2, 1, 72, 0, 0)
    struct.pack_into("<II16sQQQQIIII", data, 32, 0x19, 72, b"__TEXT", 0, 4096, 0,
                     len(data) + len(payload), 7, 5, 0, 0)
    return data + payload


def pe(payload=b""):
    data = bytearray(512)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 68, 0x8664, 1, 0, 0, 0, 112, 2)
    struct.pack_into("<H", data, 88, 0x20B)
    struct.pack_into("<II", data, 120, 4096, 512)
    struct.pack_into("<I", data, 144, 8192)
    struct.pack_into("<I", data, 148, 512)
    data[200:208] = b".text\0\0\0"
    struct.pack_into("<IIII", data, 208, len(payload), 4096, len(payload), 512)
    return data + payload


class BinaryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.policy = self.root / "policy.json"
        self.policy.write_text(json.dumps({"version": 1, "rules": [{"id": "N001", "pattern": "violetwidget"}]}))

    def scan(self, data, *options, name="sample"):
        path = self.root / name
        path.write_bytes(data)
        result = subprocess.run([sys.executable, str(SCRIPT), str(path), "--policy", str(self.policy), *options],
                                capture_output=True, text=True, timeout=15)
        self.assertEqual(result.stderr, "")
        return result.returncode, json.loads(result.stdout), result.stdout

    def test_supported_headers_are_evidence_not_clearance(self):
        for factory, expected in ((elf, "ELF64"), (macho, "MachO64"), (pe, "PE32+")):
            with self.subTest(format=expected):
                code, report, _ = self.scan(factory())
                self.assertEqual(code, 0)
                self.assertEqual(report["format"], expected)
                self.assertEqual(report["status"], "evidence_only")
                self.assertFalse(report["naming_clearance"])
                self.assertIn("visual_resources_not_cleared", report["unresolved"])

    def test_ascii_raw_and_path_matches_are_redacted(self):
        code, report, text = self.scan(elf(b"violetwidget\0"), name="violetwidget.bin")
        self.assertEqual(code, 1)
        self.assertNotIn("violetwidget", text.lower())
        self.assertEqual({item["rule_id"] for item in report["findings"]}, {"N001"})
        self.assertTrue({"raw_latin1", "ascii_string", "input_path"}.issubset({item["field"] for item in report["findings"]}))

    def test_utf16_both_byte_orders_and_alignments(self):
        for encoding in ("utf-16-le", "utf-16-be"):
            for pad in (b"", b"x"):
                with self.subTest(encoding=encoding, pad=pad):
                    code, report, _ = self.scan(macho(pad + "violetwidget".encode(encoding)))
                    self.assertEqual(code, 1)
                    self.assertIn(encoding, {item["field"] for item in report["findings"]})

    def test_section_metadata_is_separately_scanned(self):
        data = macho()
        data[40:56] = b"violetwidget\0\0\0\0"
        code, report, _ = self.scan(data)
        self.assertEqual(code, 1)
        self.assertIn("metadata", {item["field"] for item in report["findings"]})

    def test_truncated_and_unknown_formats(self):
        for data in (b"", b"\x89PNG\r\n", b"MZ", b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe"):
            with self.subTest(data=data):
                code, report, _ = self.scan(data)
                self.assertEqual(code, 2)
                self.assertFalse(report["naming_clearance"])

    def test_elf_out_of_range_and_bad_entry_size(self):
        for offset, layout, value in ((32, "<Q", 2**63), (54, "<H", 1), (96, "<Q", 99999)):
            data = elf()
            struct.pack_into(layout, data, offset, value)
            self.assertEqual(self.scan(data)[0], 2)

    def test_macho_command_bounds_and_count(self):
        for offset, value in ((20, 10000), (36, 4), (16, 2), (96, 10000)):
            data = macho()
            struct.pack_into("<I", data, offset, value)
            self.assertEqual(self.scan(data)[0], 2)

    def test_pe_invalid_signature_section_and_directory(self):
        for offset, value in ((60, 99999), (148, 0), (220, 99999), (196, 17), (124, 3), (144, 1)):
            data = pe(b"content")
            struct.pack_into("<I", data, offset, value)
            self.assertEqual(self.scan(data)[0], 2)

    def test_macho_path_offset_must_remain_inside_command(self):
        data = bytearray(56)
        struct.pack_into("<IiiIIIII", data, 0, 0xFEEDFACF, 0x100000C, 0, 2, 1, 24, 0, 0)
        struct.pack_into("<III", data, 32, 0x8000001C, 24, 64)
        self.assertEqual(self.scan(data)[0], 2)

    def test_limits_fail_closed(self):
        for options in (("--max-bytes", "1"), ("--max-records", "1"), ("--max-findings", "1")):
            code, report, _ = self.scan(elf(b"violetwidget\0violetwidget\0"), *options)
            self.assertEqual(code, 2)
            self.assertEqual(report["status"], "incomplete")

    def test_policy_invalid_is_neutral(self):
        self.policy.write_text('{"version": 1, "rules": []}')
        code, report, text = self.scan(elf(), name="violetwidget")
        self.assertEqual(code, 2)
        self.assertNotIn("violetwidget", text)
        self.assertFalse(report["naming_clearance"])

    def test_policy_and_binary_digests_are_bound(self):
        _, first, _ = self.scan(elf())
        _, second, _ = self.scan(elf(b"changed"))
        self.assertEqual(first["policy_sha256"], second["policy_sha256"])
        self.assertNotEqual(first["sha256"], second["sha256"])
        self.policy.write_text(json.dumps({"version": 1, "rules": [{"id": "N002", "pattern": "other"}]}))
        _, third, _ = self.scan(elf())
        self.assertNotEqual(first["policy_sha256"], third["policy_sha256"])

    @unittest.skipUnless(hasattr(os, "mkfifo"), "requires FIFO support")
    def test_fifo_is_rejected_without_blocking(self):
        path = self.root / "pipe"
        os.mkfifo(path)
        result = subprocess.run([sys.executable, str(SCRIPT), str(path), "--policy", str(self.policy)],
                                capture_output=True, text=True, timeout=3)
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
