#!/usr/bin/env python3
"""Collect dependency notice evidence externally and generate reviewed notice bundles (Python 3.11+)."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib

from spdx import ExpressionError, alternatives, election

TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "aarch64-apple-darwin")
KNOWN_LICENSES = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib",
                  "Unicode-3.0", "Unicode-DFS-2016", "BSL-1.0", "CC0-1.0", "Unlicense"}
BUNDLE_ROOT = "licenses/rust"
BUNDLE_MANIFEST = f"{BUNDLE_ROOT}/manifest.json"
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
    try:
        options = alternatives(expression)
    except ExpressionError:
        return ["license_expression_invalid"]
    identifiers = {part for option in options for term in option for part in term.split(" WITH ")}
    return ["license_identifier_or_exception_review_required"] if identifiers - KNOWN_LICENSES else []


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


def cargo(root, *args):
    return subprocess.check_output(["cargo", *args], cwd=root, text=True)


def target_snapshot(root, target):
    """Read one target's metadata and release-root graphs from the locked offline workspace."""
    metadata = json.loads(cargo(root, "metadata", "--format-version", "1", "--locked", "--offline",
                                "--filter-platform", target))
    trees = [cargo(root, "tree", "--locked", "--offline", "--package", "f5y", "--package", "foundry-tools",
                   "--target", target, "--edges", edges, "--prefix", "depth", "--format", "{p}|{f}", "--charset", "ascii")
             for edges in ("normal,no-proc-macro", "normal,build")]
    return metadata, *trees


def target_graph(root, target):
    metadata, tree, build_tree = target_snapshot(root, target)
    return {"runtime": graph_packages(tree, metadata)[1], "build_context": graph_packages(build_tree, metadata)[1]}


def collect(root, output):
    root, output = root.resolve(), output.resolve()
    if output == root or root in output.parents:
        raise InventoryError("Notice evidence must remain outside the repository")
    if output.exists():
        raise InventoryError("Use a new external output directory")
    lock_bytes = (root / "Cargo.lock").read_bytes()
    data = {target: target_snapshot(root, target) for target in TARGETS}
    result, texts = inventory(lock_bytes, data, cargo(root, "--version").strip())
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


def elected_terms(expression, elected):
    try:
        return election(expression, elected)
    except ExpressionError as error:
        raise InventoryError(str(error)) from error


def require_texts(terms, files):
    if not terms <= {item["covers"] for item in files}:
        raise InventoryError("Elected license text is missing")


def elected_files(evidence, review):
    """Select reviewed texts for one package; every collected file must be classified."""
    terms, offered = elected_terms(evidence["expression"], review.get("elected"))
    collected = {item["source_path"]: item for item in evidence["files"]}
    reviewed = {item["source_path"]: item for item in review.get("files", [])}
    if len(reviewed) != len(review.get("files", [])) or set(reviewed) != set(collected):
        raise InventoryError("Reviewed notice files differ from collected evidence")
    selected = []
    for path, item in sorted(reviewed.items()):
        if item["sha256"] != collected[path]["sha256"]:
            raise InventoryError("Reviewed notice hash differs from collected evidence")
        if item["covers"] != "notice" and item["covers"] not in offered:
            raise InventoryError("Reviewed notice covers a license outside the expression")
        if item["covers"] == "notice" or item["covers"] in terms:
            selected.append({"path": collected[path]["path"], "sha256": item["sha256"], "covers": item["covers"]})
    require_texts(terms, selected)
    return selected


def generate(lock_bytes, evidence_dir, review_bytes, output):
    """Write elected texts and a hashed bundle manifest from external evidence and review."""
    evidence = json.loads((evidence_dir / "manifest.json").read_bytes())
    texts = {item["path"]: (evidence_dir / item["path"]).read_bytes()
             for package in evidence["packages"] for item in package["files"]}
    verify_inventory(evidence, texts, lock_bytes, evidence["graphs"])
    review = json.loads(review_bytes)
    if (review.get("schema"), review.get("lock_sha256"), review.get("graph_sha256")) != (
            1, evidence["lock_sha256"], evidence["graph_sha256"]):
        raise InventoryError("Review record does not match the collected lock and graph")
    reviews = {(item["name"], item["version"], item["source"]): item for item in review["packages"]}
    # Workspace packages carry no dependency notices; the root license covers them.
    packages = {(item["name"], item["version"], item["source"]): item
                for item in evidence["packages"] if item["source"] is not None}
    if len(reviews) != len(review["packages"]) or set(reviews) != set(packages):
        raise InventoryError("Review record packages differ from the collected graph")
    records = []
    for key, package in sorted(packages.items(), key=lambda item: repr(item[0])):
        item = reviews[key]
        if (item.get("package_sha256"), item.get("expression")) != (package.get("package_sha256"), package["expression"]):
            raise InventoryError("Review record package checksum or expression changed")
        records.append({"name": key[0], "version": key[1], "source": key[2], "package_sha256": item["package_sha256"],
                        "expression": package["expression"], "elected": item["elected"],
                        "files": elected_files(package, item)})
    bundle = encoded({"schema": 1, "lock_sha256": evidence["lock_sha256"], "graph_sha256": evidence["graph_sha256"],
                      "graphs": evidence["graphs"], "review_sha256": digest(review_bytes), "packages": records})
    files = {item["path"]: texts[item["path"]] for record in records for item in record["files"]}
    verify_bundle(bundle, files, lock_bytes)
    if (output / BUNDLE_ROOT).exists():
        raise InventoryError("Remove the previous dependency notice bundle first")
    for path, content in {**files, BUNDLE_MANIFEST: bundle}.items():
        destination = output / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content)
    return json.loads(bundle)


def verify_bundle(manifest_bytes, files, lock_bytes):
    """Require exact lock, target graph and notice-file parity for a committed bundle."""
    bundle = json.loads(manifest_bytes)
    if manifest_bytes != encoded(bundle) or bundle.get("schema") != 1:
        raise InventoryError("Bundle manifest is not canonical")
    if bundle["lock_sha256"] != digest(lock_bytes):
        raise InventoryError("Bundle manifest does not match the lock")
    graphs = bundle["graphs"]
    if set(graphs) != set(TARGETS) or bundle["graph_sha256"] != digest(encoded(graphs)):
        raise InventoryError("Bundle target graphs changed")
    graph_ids = {(row["name"], row["version"], row["source"])
                 for graph in graphs.values() for rows in graph.values() for row in rows if row["source"]}
    locked, identities, expected = lock_packages(lock_bytes), set(), {}
    for package in bundle["packages"]:
        key, checksum = (package["name"], package["version"], package["source"]), package["package_sha256"]
        if key in identities or key not in locked or locked[key].get("checksum") != checksum:
            raise InventoryError("Bundle package is duplicated or differs from the lock")
        identities.add(key)
        terms, _ = elected_terms(package["expression"], package["elected"])
        if any(item["covers"] != "notice" and item["covers"] not in terms for item in package["files"]):
            raise InventoryError("Bundle includes a text for a license that was not elected")
        require_texts(terms, package["files"])
        prefix = f"{BUNDLE_ROOT}/{key[0]}-{key[1]}-{checksum[:12]}/"
        for item in package["files"]:
            if not item["path"].startswith(prefix) or ".." in PurePosixPath(item["path"]).parts or item["path"] in expected:
                raise InventoryError("Unsafe or duplicate bundle notice path")
            expected[item["path"]] = item["sha256"]
    if identities != graph_ids or any(not key[2].startswith("registry+") for key in graph_ids):
        raise InventoryError("Bundle packages differ from the registry target graph")
    if set(files) != set(expected):
        raise InventoryError("Bundle notice files are missing or unexpected")
    if any(not files[path].strip() or digest(files[path]) != sha for path, sha in expected.items()):
        raise InventoryError("Bundle notice bytes are empty or changed")
    return bundle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True,
                        help="New external evidence directory, or the bundle root when generating")
    parser.add_argument("--evidence", type=Path, help="External collection to generate the bundle from")
    parser.add_argument("--review", type=Path, help="External review record electing a license per package")
    args = parser.parse_args()
    if (args.evidence is None) != (args.review is None):
        parser.error("--evidence and --review must be supplied together")
    if args.review:
        bundle = generate((args.root / "Cargo.lock").read_bytes(), args.evidence, args.review.read_bytes(), args.output)
        print(json.dumps({"packages": len(bundle["packages"]), "lock_sha256": bundle["lock_sha256"]}))
        return 0
    result = collect(args.root, args.output)
    print(json.dumps({"status": result["status"], "packages": len(result["packages"]),
                      "lock_sha256": result["lock_sha256"]}))
    return 2  # Evidence collection is not naming or rights approval.


if __name__ == "__main__":
    raise SystemExit(main())
