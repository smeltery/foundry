#!/usr/bin/env python3
"""Supplemental executable coverage evidence; never grants total naming clearance."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import sys

from naming import ScanError, policy_rules


class Executable:
    def __init__(self, data, limit):
        self.data, self.limit, self.regions, self.records = data, limit, [], 0
        self.view, self.metadata_bytes = memoryview(data), 0

    def bounded(self, offset, size):
        if offset < 0 or size < 0 or offset > len(self.data) or size > len(self.data) - offset:
            raise ScanError("executable_range_invalid")
        return self.view[offset:offset + size]

    def unpack(self, layout, offset):
        return struct.unpack(layout, self.bounded(offset, struct.calcsize(layout)))

    def count(self, count):
        self.records += count
        if self.records > self.limit:
            raise ScanError("structure_limit_exceeded")

    def region(self, offset, size):
        self.bounded(offset, size)
        self.metadata_bytes += size
        if self.metadata_bytes > len(self.data) * 4:
            raise ScanError("metadata_work_limit_exceeded")
        self.regions.append((offset, size))

    def string(self, offset, end):
        if offset < 0 or end < offset or end > len(self.data):
            raise ScanError("metadata_string_range_invalid")
        stop = self.data.find(b"\0", offset, end)
        if stop < 0:
            raise ScanError("metadata_string_unterminated")
        self.region(offset, stop - offset)

    def elf(self):
        header = self.unpack("<16sHHIQQQIHHHHHH", 0)
        ident, kind, machine, version, _, phoff, shoff, _, size, phsize, phnum, shsize, shnum, names = header
        if (ident[:7] != b"\x7fELF\x02\x01\x01" or kind not in (2, 3)
                or machine not in (62, 183) or version != 1 or size != 64
                or not phnum or phnum == 65535 or phsize != 56 or phoff < 64):
            raise ScanError("elf_header_unsupported_or_invalid")
        self.count(phnum + shnum)
        self.bounded(phoff, phnum * phsize)
        for index in range(phnum):
            ptype, _, offset, _, _, files, memory, _ = self.unpack("<IIQQQQQQ", phoff + index * phsize)
            self.bounded(offset, files)
            if ptype == 1 and files > memory:
                raise ScanError("elf_segment_size_invalid")
            if ptype == 3:
                self.string(offset, offset + files)
        if not shnum:
            if shoff or names:
                raise ScanError("elf_extended_sections_unsupported")
            return "ELF64", machine
        if shoff < 64 or shsize != 64 or names >= shnum:
            raise ScanError("elf_section_table_invalid")
        self.bounded(shoff, shnum * shsize)
        sections = [self.unpack("<IIQQQQIIQQ", shoff + index * shsize) for index in range(shnum)]
        strings = sections[names]
        if names and strings[1] != 3:
            raise ScanError("elf_section_names_invalid")
        for section in sections:
            name, kind, _, _, offset, length, link, _, _, entry = section
            if kind != 8:
                self.bounded(offset, length)
            if name:
                if not names or name >= strings[5]:
                    raise ScanError("elf_section_name_invalid")
                self.string(strings[4] + name, strings[4] + strings[5])
            if kind in (2, 11):
                if entry != 24 or length % entry or link >= shnum or sections[link][1] != 3:
                    raise ScanError("elf_symbols_invalid")
                table = sections[link]
                self.count(length // entry)
                for pos in range(offset, offset + length, entry):
                    symbol = self.unpack("<I", pos)[0]
                    if symbol >= table[5]:
                        raise ScanError("elf_symbol_name_invalid")
                    self.string(table[4] + symbol, table[4] + table[5])
        return "ELF64", machine

    def macho(self):
        magic, cpu, _, kind, count, length, _, _ = self.unpack("<IiiIIIII", 0)
        if magic != 0xFEEDFACF or cpu not in (0x1000007, 0x100000C) or kind not in (2, 6, 8):
            raise ScanError("macho_header_unsupported_or_invalid")
        self.count(count)
        if not count:
            raise ScanError("macho_commands_missing")
        self.bounded(32, length)
        offset, end = 32, 32 + length
        for _ in range(count):
            cmd, size = self.unpack("<II", offset)
            if size < 8 or size % 8 or offset + size > end:
                raise ScanError("macho_command_invalid")
            if cmd == 0x19:
                if size < 72:
                    raise ScanError("macho_segment_invalid")
                fileoff, filesize = self.unpack("<QQ", offset + 40)
                sections = self.unpack("<I", offset + 64)[0]
                if size != 72 + sections * 80:
                    raise ScanError("macho_sections_invalid")
                self.count(sections)
                self.bounded(fileoff, filesize)
                self.region(offset + 8, 16)
                for index in range(sections):
                    start = offset + 72 + index * 80
                    self.region(start, 32)
                    amount, location = self.unpack("<QI", start + 40)
                    flags = self.unpack("<I", start + 64)[0] & 0xFF
                    if flags not in (1, 12, 18):
                        self.bounded(location, amount)
            elif cmd == 2:
                if size != 24:
                    raise ScanError("macho_symbols_invalid")
                symbols, number, strings, amount = self.unpack("<IIII", offset + 8)
                self.count(number)
                self.bounded(symbols, number * 16)
                self.bounded(strings, amount)
                for index in range(number):
                    name = self.unpack("<I", symbols + index * 16)[0]
                    if name >= amount:
                        raise ScanError("macho_symbol_name_invalid")
                    self.string(strings + name, strings + amount)
            elif cmd in (12, 13, 0x80000018, 0x8000001F, 0x20, 0x80000023, 0x8000001C):
                minimum = 12 if cmd == 0x8000001C else 24
                if size < minimum:
                    raise ScanError("macho_path_command_invalid")
                name = self.unpack("<I", offset + 8)[0]
                if name < minimum or name >= size:
                    raise ScanError("macho_path_offset_invalid")
                self.string(offset + name, offset + size)
            elif cmd in (0x1D, 0x26, 0x29, 0x2B, 0x2E, 0x80000033, 0x80000034):
                if size != 16:
                    raise ScanError("macho_linkedit_invalid")
                self.bounded(*self.unpack("<II", offset + 8))
            offset += size
        if offset != end:
            raise ScanError("macho_command_count_mismatch")
        return "MachO64", cpu

    def pe(self):
        pe = self.unpack("<I", 60)[0]
        if pe < 64 or self.bounded(pe, 4) != b"PE\0\0":
            raise ScanError("pe_signature_invalid")
        machine, count, _, symbols, number, optional, flags = self.unpack("<HHIIIHH", pe + 4)
        if machine not in (0x8664, 0xAA64) or not count or optional < 112 or not flags & 2:
            raise ScanError("pe_header_unsupported_or_invalid")
        self.count(count + number)
        start = pe + 24
        self.bounded(start, optional)
        if self.unpack("<H", start)[0] != 0x20B:
            raise ScanError("pe_optional_header_unsupported")
        directories = self.unpack("<I", start + 108)[0]
        if directories > 16 or 112 + directories * 8 > optional:
            raise ScanError("pe_directories_invalid")
        section_table = start + optional
        self.bounded(section_table, count * 40)
        headers = self.unpack("<I", start + 60)[0]
        section_align, file_align = self.unpack("<II", start + 32)
        image_size = self.unpack("<I", start + 56)[0]
        if (not file_align or file_align & (file_align - 1) or section_align < file_align
                or not image_size or headers > image_size):
            raise ScanError("pe_alignment_or_image_size_invalid")
        if headers < section_table + count * 40 or headers > len(self.data):
            raise ScanError("pe_header_size_invalid")
        sections = []
        for index in range(count):
            section = section_table + index * 40
            self.region(section, 8)
            virtual_size, rva, amount, offset = self.unpack("<IIII", section + 8)
            if (rva + max(virtual_size, amount) > image_size or (amount and offset < headers)):
                raise ScanError("pe_section_layout_invalid")
            self.bounded(offset, amount)
            sections.append((rva, virtual_size, offset, amount))
        for index in range(directories):
            rva, amount = self.unpack("<II", start + 112 + index * 8)
            if not amount:
                continue
            if index == 4:
                self.bounded(rva, amount)
            elif not any(base <= rva and rva - base + amount <= size
                         for base, _, _, size in sections) and rva + amount > headers:
                raise ScanError("pe_directory_range_invalid")
        if number:
            self.bounded(symbols, number * 18)
            table = symbols + number * 18
            amount = self.unpack("<I", table)[0]
            if amount < 4:
                raise ScanError("pe_string_table_invalid")
            self.region(table + 4, amount - 4)
        return "PE32+", machine

    def inspect(self):
        if self.data.startswith(b"\x7fELF"):
            return self.elf()
        if self.data.startswith(b"\xcf\xfa\xed\xfe"):
            return self.macho()
        if self.data.startswith(b"MZ"):
            return self.pe()
        raise ScanError("executable_format_unsupported")


def inspect(data, label, rules, max_records=65536, max_findings=1000):
    executable = Executable(data, max_records)
    format_name, machine = executable.inspect()
    findings, scans, scanned_chars = [], 0, 0

    def scan(text, field, offset=None):
        nonlocal scans, scanned_chars
        scans += 1
        scanned_chars += len(text)
        if scanned_chars > max(len(data) * 12, 4096):
            raise ScanError("scan_work_limit_exceeded")
        for rule, regex in rules:
            for _ in regex.finditer(text):
                if len(findings) >= max_findings:
                    raise ScanError("finding_limit_exceeded")
                findings.append({"rule_id": rule, "field": field, "region_offset": offset})

    scan(label, "input_path")
    scan(data.decode("latin1"), "raw_latin1", 0)
    for match in re.finditer(rb"[\x20-\x7e]{4,}", data):
        executable.count(1)
        scan(match.group().decode("ascii"), "ascii_string", match.start())
    for encoding in ("utf-16-le", "utf-16-be"):
        for alignment in (0, 1):
            size = (len(data) - alignment) // 2 * 2
            scan(data[alignment:alignment + size].decode(encoding, errors="replace"), encoding, alignment)
    for offset, size in executable.regions:
        scan(bytes(executable.bounded(offset, size)).decode("utf-8", errors="replace"), "metadata", offset)
    return {"schema": 1, "inspector_version": 1, "status": "findings" if findings else "evidence_only",
            "naming_clearance": False, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data),
            "format": format_name, "machine": machine, "findings": findings, "scan_passes": scans,
            "validated_records": executable.records, "metadata_regions": len(executable.regions),
            "coverage": ["supported_header_bounds", "raw_latin1", "ascii_strings", "utf16_both_alignments",
                         "input_path", "selected_format_metadata"],
            "unresolved": ["visual_resources_not_cleared", "compressed_or_embedded_content_not_decoded",
                           "format_semantics_partial", "computed_runtime_output_not_inspected"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path)
    policy = parser.add_mutually_exclusive_group()
    policy.add_argument("--policy", type=Path)
    policy.add_argument("--policy-json-env", default="FOUNDRY_NAMING_POLICY")
    parser.add_argument("--max-bytes", type=int, default=32 * 1024 * 1024)
    parser.add_argument("--max-records", type=int, default=65536)
    parser.add_argument("--max-findings", type=int, default=1000)
    args = parser.parse_args()
    try:
        if min(args.max_bytes, args.max_records, args.max_findings) <= 0:
            raise ScanError("limits_invalid")
        rules = policy_rules(args)
        descriptor = os.open(args.path, os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(descriptor, "rb") as source:
            if args.path.is_symlink() or not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                raise ScanError("regular_file_required")
            data = source.read(args.max_bytes + 1)
        if len(data) > args.max_bytes:
            raise ScanError("file_limit_exceeded")
        result = inspect(data, str(args.path), rules, args.max_records, args.max_findings)
        canonical = json.dumps([(name, regex.pattern, regex.flags) for name, regex in rules], separators=(",", ":"))
        result["policy_sha256"] = hashlib.sha256(canonical.encode()).hexdigest()
        result["inspector_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        code = 1 if result["findings"] else 0
    except ScanError as error:
        result, code = {"schema": 1, "status": "incomplete", "naming_clearance": False, "errors": [str(error)]}, 2
    except (OSError, ValueError, struct.error, OverflowError):
        result, code = {"schema": 1, "status": "incomplete", "naming_clearance": False, "errors": ["inspection_failed"]}, 2
    print(json.dumps(result, sort_keys=True))
    return code


if __name__ == "__main__":
    sys.exit(main())
