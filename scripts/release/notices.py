#!/usr/bin/env python3
"""Collect unapproved dependency notice evidence outside the repository (Python 3.11+)."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib

TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "aarch64-apple-darwin")
KNOWN_LICENSES = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib",
                  "Unicode-3.0", "Unicode-DFS-2016", "BSL-1.0", "CC0-1.0", "Unlicense"}
NOTICE_NAME = re.compile(r"(?:LICENSE|LICENCE|COPYING|COPYRIGHT|NOTICE|AUTHORS)(?:[._-].*)?", re.I)


class InventoryError(ValueError):
    """Evidence is inconsistent or cannot be safely collected."""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()


def license_findings(expression):
    """Check a conservative expression grammar, never grant distribution approval."""
    if not expression:
        return ["license_expression_missing"]
    tokens = re.findall(r"[A-Za-z0-9.+-]+|[()]", expression)
    if "".join(tokens) != re.sub(r"\s+", "", expression):
        return ["license_expression_invalid"]
    position, identifiers = 0, []

    def atom():
        nonlocal position
        if position >= len(tokens):
            raise InventoryError("Missing expression term")
        token = tokens[position]
        position += 1
        if token == "(":
            sequence()
            if position >= len(tokens) or tokens[position] != ")":
                raise InventoryError("Unbalanced expression")
            position += 1
        elif token in {"AND", "OR", "WITH", ")"}:
            raise InventoryError("Unexpected expression term")
        else:
            identifiers.append(token)
            if position < len(tokens) and tokens[position] == "WITH":
                position += 1
                if position >= len(tokens) or tokens[position] in {"AND", "OR", "WITH", "(", ")"}:
                    raise InventoryError("Missing exception")
                identifiers.append(tokens[position])
                position += 1

    def sequence():
        nonlocal position
        atom()
        while position < len(tokens) and tokens[position] in {"AND", "OR"}:
            position += 1
            atom()

    try:
        sequence()
        if position != len(tokens):
            raise InventoryError("Trailing expression terms")
    except InventoryError:
        return ["license_expression_invalid"]
    return ["license_identifier_or_exception_review_required"] if set(identifiers) - KNOWN_LICENSES else []


def lock_packages(lock_bytes):
    entries = {}
    for item in tomllib.loads(lock_bytes.decode())["package"]:
        key = (item["name"], item["version"], item.get("source"))
        if key in entries:
            raise InventoryError("Duplicate locked package")
        entries[key] = item
    return entries


def graph_packages(tree, metadata):
    """Resolve Cargo tree names to unique metadata identities; preserve depth/features."""
    index, rows = {}, []
    for package in metadata["packages"]:
        index.setdefault((package["name"], package["version"]), []).append(package)
    selected = {}
    for line in tree.splitlines():
        if not line:
            continue
        match = re.fullmatch(r"(\d+)([A-Za-z0-9_-]+) v([^ |]+)(?: [^|]*)?\|(.*)", line)
        if match is None:
            raise InventoryError("Unexpected target graph format")
        depth, name, version, features = match.groups()
        matches = index.get((name, version), [])
        if len(matches) != 1:
            raise InventoryError("Missing or ambiguous package identity")
        package = matches[0]
        key = (name, version, package.get("source"))
        selected[key] = package
        rows.append({"depth": int(depth), "name": name, "version": version,
                     "source": key[2], "features": sorted(filter(None, features.removesuffix(" (*)").split(",")))})
    if not rows or rows[0]["depth"] != 0:
        raise InventoryError("Empty or invalid target graph")
    return selected, rows


def package_evidence(package, locked):
    key = (package["name"], package["version"], package.get("source"))
    if key not in locked:
        raise InventoryError("Metadata package does not match lock")
    record = {"name": key[0], "version": key[1], "source": key[2],
              "expression": package.get("license"), "files": [], "findings": []}
    findings = record["findings"]
    if not key[2]:
        findings.append("workspace_or_path_notice_review_required")
        return record, {}
    if not key[2].startswith("registry+"):
        findings.append("unsupported_source_review_required")
        return record, {}
    checksum = locked[key].get("checksum")
    if not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum):
        raise InventoryError("Missing package checksum")
    record["package_sha256"] = checksum
    findings.extend(license_findings(package.get("license")))
    root = Path(package["manifest_path"]).parent
    archive = root.parents[2] / "cache" / root.parent.name / (root.name + ".crate")
    if not archive.is_file():
        findings.append("source_archive_missing")
        return record, {}
    if digest(archive.read_bytes()) != checksum:
        raise InventoryError("Source package checksum mismatch")
    collected = {}
    with tarfile.open(archive, "r:gz") as bundle:
        members = {}
        for item in bundle.getmembers():
            path = PurePosixPath(item.name)
            if (path.is_absolute() or ".." in path.parts or not path.parts or path.parts[0] != root.name
                    or "\\" in item.name or ":" in item.name):
                raise InventoryError("Unsafe source archive path")
            relative = PurePosixPath(*path.parts[1:]).as_posix()
            if item.isdir():
                continue
            if relative in members or not item.isfile():
                raise InventoryError("Duplicate or nonregular source archive member")
            members[relative] = item
        manifest = members.get("Cargo.toml")
        if manifest is None or bundle.extractfile(manifest).read() != Path(package["manifest_path"]).read_bytes():
            raise InventoryError("Unpacked package manifest changed")
        candidates = {path for path in members if NOTICE_NAME.fullmatch(PurePosixPath(path).name)}
        declared = package.get("license_file")
        if declared:
            if declared not in members:
                findings.append("declared_license_file_missing")
            else:
                candidates.add(declared)
        for path in sorted(candidates):
            data = bundle.extractfile(members[path]).read()
            local = root / path
            if local.is_symlink() or not local.is_file() or local.read_bytes() != data:
                raise InventoryError("Unpacked notice changed or missing")
            if not data.strip():
                findings.append("empty_notice_text")
            destination = f"licenses/rust/{key[0]}-{key[1]}-{checksum[:12]}/{path}"
            collected[destination] = data
            record["files"].append({"path": destination, "source_path": path, "sha256": digest(data)})
    if not candidates:
        findings.append("notice_text_missing")
    findings.extend(("expression_to_text_review_required", "copyright_and_notice_obligations_review_required"))
    return record, collected


def inventory(lock_bytes, target_data, toolchain):
    locked, packages, graphs, usages = lock_packages(lock_bytes), {}, {}, {}
    for target, (metadata, tree, build_tree) in sorted(target_data.items()):
        selected, rows = graph_packages(tree, metadata)
        build_selected, build_rows = graph_packages(build_tree, metadata)
        graphs[target] = {"runtime": rows, "build_context": build_rows}
        for key, package in {**selected, **build_selected}.items():
            if key in packages and packages[key] != package:
                raise InventoryError("Package metadata differs across target snapshots")
            packages[key] = package
            usages.setdefault(key, []).append({"target": target, "runtime": key in selected})
    records, texts = [], {}
    for key, package in sorted(packages.items(), key=lambda item: repr(item[0])):
        record, collected = package_evidence(package, locked)
        if set(texts) & set(collected):
            raise InventoryError("Duplicate notice destination")
        record["targets"] = usages[key]
        records.append(record)
        texts.update(collected)
    result = {"schema": 1, "status": "review_required", "lock_sha256": digest(lock_bytes),
              "toolchain": toolchain, "feature_selection": "default", "graphs": graphs,
              "graph_sha256": digest(encoded(graphs)), "packages": records}
    return result, texts


def verify_inventory(manifest, texts, lock_bytes, graphs):
    if manifest["lock_sha256"] != digest(lock_bytes):
        raise InventoryError("Lock changed")
    if manifest["graph_sha256"] != digest(encoded(graphs)) or manifest["graphs"] != graphs:
        raise InventoryError("Target graph changed")
    expected, identities, locked = {}, set(), lock_packages(lock_bytes)
    graph_ids = {(row["name"], row["version"], row["source"])
                 for graph in graphs.values() for rows in graph.values() for row in rows}
    for package in manifest["packages"]:
        key = (package["name"], package["version"], package["source"])
        if key in identities or key not in locked:
            raise InventoryError("Duplicate or unlocked inventory package")
        identities.add(key)
        if key[2] and key[2].startswith("registry+") and package.get("package_sha256") != locked[key].get("checksum"):
            raise InventoryError("Inventory source checksum changed")
        for item in package["files"]:
            if item["path"] in expected:
                raise InventoryError("Duplicate notice inventory entry")
            expected[item["path"]] = item["sha256"]
    if identities != graph_ids:
        raise InventoryError("Target graph package coverage differs")
    if set(expected) != set(texts) or any(digest(texts[path]) != sha for path, sha in expected.items()):
        raise InventoryError("Notice bytes or inventory changed")


def collect(root, output):
    root, output = root.resolve(), output.resolve()
    if output == root or root in output.parents:
        raise InventoryError("Notice evidence must remain outside the repository")
    if output.exists():
        raise InventoryError("Use a new external output directory")
    lock_bytes = (root / "Cargo.lock").read_bytes()

    def cargo(*args):
        return subprocess.check_output(["cargo", *args], cwd=root, text=True)

    data = {}
    for target in TARGETS:
        metadata = json.loads(cargo("metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", target))
        trees = [cargo("tree", "--locked", "--offline", "--package", "f5y", "--package", "foundry-tools", "--target", target,
                       "--edges", edges, "--prefix", "depth", "--format", "{p}|{f}", "--charset", "ascii")
                 for edges in ("normal,no-proc-macro", "normal,build")]
        data[target] = (metadata, *trees)
    result, texts = inventory(lock_bytes, data, cargo("--version").strip())
    if (root / "Cargo.lock").read_bytes() != lock_bytes:
        raise InventoryError("Lock changed during collection")
    verify_inventory(result, texts, lock_bytes, result["graphs"])
    output.mkdir(parents=True)
    for path, content in texts.items():
        destination = output / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content)
    (output / "manifest.json").write_bytes(encoded(result))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = collect(args.root, args.output)
    print(json.dumps({"status": result["status"], "packages": len(result["packages"]),
                      "lock_sha256": result["lock_sha256"]}))
    return 2  # Evidence collection is not naming or rights approval.


if __name__ == "__main__":
    raise SystemExit(main())
