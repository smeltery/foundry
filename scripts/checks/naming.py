#!/usr/bin/env python3
"""Scan indexed Git blobs, reachable history, or release files using external JSON.

Policy: {"version": 1, "rules": [{"id": "N001", "pattern": "example"}]}.
Patterns use Python regex with IGNORECASE; policies must come from a trusted source.
Exit codes: 0 fully inspected/clean, 1 matches, 2 invalid input/incomplete inspection.
No network requests are made. Binary/undecodable assets require separate inspection.
"""

import argparse
import bz2
import gzip
import hashlib
import lzma
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
import tarfile
import tempfile
import zipfile
import zlib


class ScanError(Exception):
    """A neutral diagnostic safe to print without leaking scanned content."""


def policy_rules(args):
    try:
        if args.policy:
            with open(args.policy, "rb") as source:
                raw = source.read(1024 * 1024 + 1)
        else:
            raw = os.environ.get(args.policy_json_env, "").encode()
        if not raw or len(raw) > 1024 * 1024:
            raise ScanError("policy_missing_or_oversized")
        policy = json.loads(raw)
        if not isinstance(policy, dict) or type(policy.get("version")) is not int or policy["version"] != 1:
            raise ScanError("policy_schema_invalid")
        reviews = policy.get("svg_reviews", [])
        if not isinstance(reviews, list) or len(reviews) > 1000:
            raise ScanError("policy_svg_reviews_invalid")
        args.svg_reviews = set()
        for review in reviews:
            if (not isinstance(review, dict) or set(review) != {"sha256", "evidence"}
                    or not isinstance(review["sha256"], str)
                    or not re.fullmatch(r"[0-9a-f]{64}", review["sha256"])
                    or not isinstance(review["evidence"], str)
                    or not re.fullmatch(r"[A-Za-z0-9._-]{1,128}", review["evidence"])
                    or review["sha256"] in args.svg_reviews):
                raise ScanError("policy_svg_review_invalid")
            args.svg_reviews.add(review["sha256"])
        rules = policy.get("rules")
        if not isinstance(rules, list) or not 1 <= len(rules) <= 100:
            raise ScanError("policy_rules_invalid")
        compiled = []
        ids = set()
        for rule in rules:
            if not isinstance(rule, dict) or set(rule) != {"id", "pattern"}:
                raise ScanError("policy_rule_invalid")
            rule_id, pattern = rule["id"], rule["pattern"]
            if not isinstance(rule_id, str) or not re.fullmatch(r"N[0-9]{3,6}", rule_id):
                raise ScanError("policy_rule_id_invalid")
            if rule_id in ids or not isinstance(pattern, str) or not 1 <= len(pattern) <= 4096:
                raise ScanError("policy_rule_invalid")
            regex = re.compile(pattern, re.IGNORECASE)
            if regex.search(""):
                raise ScanError("policy_empty_match_invalid")
            ids.add(rule_id)
            compiled.append((rule_id, regex))
        return compiled
    except (OSError, ValueError, UnicodeError, re.error, RecursionError):
        raise ScanError("policy_unreadable_or_malformed") from None


class ArchiveReader:
    def __init__(self, stream, scanner):
        self.stream, self.scanner = stream, scanner

    def read(self, size):
        remaining = self.scanner.args.max_total_bytes - self.scanner.expanded
        data = self.stream.read(min(size, remaining + 1))
        self.scanner.expanded += len(data)
        if self.scanner.expanded > self.scanner.args.max_total_bytes:
            raise ScanError("archive_expansion_limit_exceeded")
        return data


class Scanner:
    def __init__(self, args, rules):
        self.args, self.rules = args, rules
        self.findings, self.uninspected = [], []
        self.files = self.bytes = self.texts = self.matches = self.expanded = 0
        self.seen_blobs = set()

    def location(self, value):
        # Mask the union of all matches before emitting paths, including overlaps.
        spans = sorted((m.start(), m.end()) for _, rx in self.rules for m in rx.finditer(value))
        output, end = [], 0
        for start, stop in spans:
            if stop <= end:
                continue
            if start >= end:
                output.extend([value[end:start], "[redacted]"])
            end = stop
        return "".join(output) + value[end:]

    def scan(self, value, location, field):
        for rule_id, regex in self.rules:
            count = 0
            first = None
            for match in regex.finditer(value):
                count += 1
                if first is None:
                    first = match.start()
            if count:
                self.matches += count
                record = {"rule_id": rule_id, "path": self.location(location),
                          "field": field, "count": count}
                if field == "text":
                    record["first_line"] = value.count("\n", 0, first) + 1
                self.findings.append(record)

    def skip(self, location, code):
        self.uninspected.append({"path": self.location(location), "code": code})

    def visit(self, location):
        self.files += 1
        if self.files > self.args.max_files:
            raise ScanError("file_limit_exceeded")
        self.scan(location, location, "path")

    def read(self, stream, size, location):
        if size > self.args.max_bytes:
            self.skip(location, "file_size_limit")
            return None
        remaining = self.args.max_total_bytes - self.bytes
        if size > remaining:
            raise ScanError("total_size_limit_exceeded")
        data = stream.read(min(self.args.max_bytes, remaining) + 1)
        self.bytes += len(data)
        if len(data) > self.args.max_bytes or self.bytes > self.args.max_total_bytes:
            raise ScanError("expanded_size_limit_exceeded")
        return data

    def content(self, data, location, depth=0):
        if data is None:
            return
        archive_name = location.lower().endswith((".zip", ".tar", ".tgz", ".tar.gz", ".tar.bz2", ".tar.xz"))
        if archive_name or zipfile.is_zipfile(io.BytesIO(data)):
            if depth >= self.args.max_depth:
                self.skip(location, "archive_depth_limit")
                return
            self.archive(data, location, depth + 1)
            return
        if location.lower().endswith((".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".icns", ".svg", ".pdf")):
            reviewed_svg = (location.lower().endswith(".svg")
                            and hashlib.sha256(data).hexdigest() in self.args.svg_reviews)
            if not reviewed_svg:
                self.skip(location, "visual_asset_requires_external_inspection")
            # Textual vector assets still receive a text scan, without claiming visual coverage.
        if b"\0" in data:
            self.skip(location, "binary_requires_external_inspection")
            return
        try:
            value = data.decode("utf-8")
        except UnicodeError:
            self.skip(location, "encoding_requires_external_inspection")
            return
        if value.startswith("version https://git-lfs.github.com/spec/v1\n"):
            self.skip(location, "external_object_not_fetched")
        self.texts += 1
        self.scan(value, location, "text")

    @staticmethod
    def safe_member(name):
        path = PurePosixPath(name)
        return bool(name) and len(name) <= 4096 and not path.is_absolute() and ".." not in path.parts and "\\" not in name and ":" not in name

    def archive(self, data, location, depth):
        try:
            if zipfile.is_zipfile(io.BytesIO(data)):
                with zipfile.ZipFile(io.BytesIO(data)) as archive:
                    entries = archive.infolist()
                    if len(entries) > self.args.max_files - self.files:
                        raise ScanError("file_limit_exceeded")
                    for entry in entries:
                        label = location + "!/" + entry.filename
                        self.visit(label)
                        if not self.safe_member(entry.filename):
                            self.skip(label, "unsafe_archive_path")
                        elif stat.S_ISLNK(entry.external_attr >> 16):
                            self.skip(label, "archive_link_not_followed")
                        elif not entry.is_dir():
                            with archive.open(entry) as source:
                                self.content(self.read(source, entry.file_size, label), label, depth)
            else:
                source = io.BytesIO(data)
                if data.startswith(b"\x1f\x8b"):
                    source = gzip.GzipFile(fileobj=source)
                elif data.startswith(b"BZh"):
                    source = bz2.BZ2File(source)
                elif data.startswith(b"\xfd7zXZ\x00"):
                    source = lzma.LZMAFile(source)
                with source, tarfile.open(fileobj=ArchiveReader(source, self), mode="r|") as archive:
                    for entry in archive:
                        label = location + "!/" + entry.name
                        self.visit(label)
                        if not self.safe_member(entry.name):
                            self.skip(label, "unsafe_archive_path")
                        elif entry.isdir():
                            continue
                        elif not entry.isfile():
                            self.skip(label, "archive_special_entry_not_followed")
                        else:
                            with archive.extractfile(entry) as source:
                                self.content(self.read(source, entry.size, label), label, depth)
        except (OSError, ValueError, EOFError, RuntimeError, NotImplementedError,
                tarfile.TarError, zipfile.BadZipFile, lzma.LZMAError, zlib.error):
            self.skip(location, "archive_unreadable_or_corrupt")

    def release(self, path):
        def file(candidate, label):
            self.visit(label)
            if candidate.is_symlink():
                self.skip(label, "filesystem_link_not_followed")
            elif candidate.is_file():
                with candidate.open("rb") as source:
                    self.content(self.read(source, os.fstat(source.fileno()).st_size, label), label)
            else:
                self.skip(label, "filesystem_special_entry")
        if path.is_symlink():
            file(path, path.name)
        elif path.is_dir():
            def failed(_):
                raise ScanError("directory_unreadable")
            for directory, folders, files in os.walk(path, followlinks=False, onerror=failed):
                for name in sorted(folders):
                    child = Path(directory) / name
                    label = child.relative_to(path).as_posix()
                    if child.is_symlink():
                        file(child, label)
                        folders.remove(name)
                    else:
                        self.visit(label)
                for name in sorted(files):
                    child = Path(directory) / name
                    file(child, child.relative_to(path).as_posix())
        elif path.exists():
            file(path, path.name)
        else:
            raise ScanError("release_path_missing")

    def git(self, *arguments, limit=32 * 1024 * 1024):
        with tempfile.TemporaryFile() as output:
            result = subprocess.run(["git", "-C", str(self.args.root), *arguments],
                                    stdout=output, stderr=subprocess.DEVNULL, timeout=60,
                                    env={**os.environ, "GIT_NO_LAZY_FETCH": "1", "GIT_NO_REPLACE_OBJECTS": "1"})
            if result.returncode:
                raise ScanError("git_command_failed")
            if output.tell() > limit:
                raise ScanError("git_output_limit_exceeded")
            output.seek(0)
            return output.read()

    def blob(self, oid, label):
        identity = (oid, Path(label).suffix.lower())
        if identity in self.seen_blobs:
            return
        self.seen_blobs.add(identity)
        size = int(self.git("cat-file", "-s", oid))
        if size > self.args.max_bytes:
            self.skip(label, "file_size_limit")
            return
        if size > self.args.max_total_bytes - self.bytes:
            raise ScanError("total_size_limit_exceeded")
        data = self.git("cat-file", "-p", oid, limit=self.args.max_bytes)
        self.bytes += len(data)
        self.content(data, label)

    def tree(self, records, revision=None):
        for record in records.split(b"\0"):
            if not record:
                continue
            header, name = record.split(b"\t", 1)
            parts = header.decode("ascii").split()
            mode, oid = parts[0], parts[2] if revision else parts[1]
            label = (revision + ":" if revision else "") + name.decode("utf-8", "backslashreplace")
            self.visit(label)
            try:
                name.decode("utf-8")
            except UnicodeError:
                self.skip(label, "path_encoding_requires_external_inspection")
            if mode == "160000":
                self.skip(label, "submodule_requires_separate_scan")
            elif mode == "120000":
                self.blob(oid, label)
                self.skip(label, "git_link_target_not_followed")
            elif not revision and parts[2] != "0":
                self.skip(label, "unmerged_index_entry")
            else:
                self.blob(oid, label)

    def tracked(self):
        self.tree(self.git("ls-files", "--stage", "-z"))

    def history(self):
        if self.git("rev-parse", "--is-shallow-repository").strip() == b"true":
            raise ScanError("shallow_history_incomplete")
        commits = self.git("rev-list", "--all", "--max-count=" + str(self.args.max_commits + 1)).splitlines()
        if len(commits) > self.args.max_commits:
            raise ScanError("commit_limit_exceeded")
        for raw in commits:
            commit = raw.decode("ascii")
            self.visit(commit + ":commit")
            self.blob(commit, commit + ":commit")
            self.tree(self.git("ls-tree", "-r", "-z", commit), commit)
        for line in self.git("for-each-ref", "--format=%(objecttype) %(objectname)", "refs/tags").splitlines():
            kind, oid = line.decode("ascii").split()
            if kind == "tag":
                self.visit(oid + ":tag")
                self.blob(oid, oid + ":tag")

    def report(self, errors):
        status = "violations" if self.matches else "incomplete" if errors or self.uninspected else "clean"
        return {"status": status, "mode": self.args.mode, "files": self.files,
                "bytes": self.bytes, "archive_expanded_bytes": self.expanded, "text_objects": self.texts, "match_count": self.matches,
                "findings": self.findings, "uninspected": self.uninspected, "errors": errors}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--policy", type=Path)
    parser.add_argument("--policy-json-env", default="F5Y_NAMING_POLICY_JSON")
    parser.add_argument("--mode", choices=("tracked", "history", "release"), default="tracked")
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--release", type=Path)
    for name, default in (("max-bytes", 8 * 1024 * 1024), ("max-total-bytes", 64 * 1024 * 1024),
                          ("max-files", 10000), ("max-commits", 1000), ("max-depth", 4)):
        parser.add_argument("--" + name, type=int, default=default)
    args = parser.parse_args(argv)
    scanner, errors = None, []
    try:
        if any(getattr(args, name) <= 0 for name in ("max_bytes", "max_total_bytes", "max_files", "max_commits", "max_depth")):
            raise ScanError("limits_must_be_positive")
        scanner = Scanner(args, policy_rules(args))
        if args.mode == "release":
            if args.release is None:
                raise ScanError("release_path_required")
            scanner.release(args.release)
        else:
            getattr(scanner, args.mode)()
    except ScanError as error:
        errors.append(str(error))
    except (OSError, ValueError, subprocess.TimeoutExpired):
        errors.append("input_unreadable_or_operation_failed")
    report = scanner.report(errors) if scanner else {"status": "incomplete", "errors": errors}
    print(json.dumps(report, sort_keys=True))
    return 1 if report["status"] == "violations" else 2 if report["status"] == "incomplete" else 0


if __name__ == "__main__":
    sys.exit(main())
