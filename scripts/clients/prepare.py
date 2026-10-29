#!/usr/bin/env python3
"""Prepare a local client distribution with an explicitly selected executable."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys


ROOT = Path(__file__).resolve().parents[2]
DISCOVERY = (
    ".agents/plugins/marketplace.json",
    ".claude-plugin/marketplace.json",
    ".cursor-plugin/marketplace.json",
)


def regular_absolute(path):
    if not path.is_absolute() or not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError("select an absolute regular file without a final symlink")
    return path.resolve(strict=True)


def copy_content(source, destination):
    if source.is_symlink():
        raise ValueError("package content must not contain symlinks")
    if source.is_dir():
        destination.mkdir()
        for child in sorted(source.iterdir()):
            copy_content(child, destination / child.name)
    elif stat.S_ISREG(source.lstat().st_mode):
        shutil.copyfile(source, destination)
    else:
        raise ValueError("package content must contain only directories and files")


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def package_files(source):
    inventory = json.loads((source / "files.json").read_text(encoding="utf-8"))
    if not isinstance(inventory, dict) or set(inventory) != {"version", "files"}:
        raise ValueError("invalid package inventory")
    files = inventory["files"]
    if (type(inventory["version"]) is not int or inventory["version"] != 1
            or not isinstance(files, list) or not files):
        raise ValueError("invalid package inventory")
    if any(not isinstance(item, str) for item in files) or len(set(files)) != len(files):
        raise ValueError("invalid package inventory entries")
    if "files.json" not in files:
        raise ValueError("package inventory must include itself")
    for name in files:
        path = PurePosixPath(name)
        if (path.is_absolute() or ".." in path.parts or "\\" in name
                or path.as_posix() != name or not path.parts):
            raise ValueError("package inventory path must be contained")
        current = source
        for part in path.parts:
            current = current / part
            if current.is_symlink():
                raise ValueError("package inventory must not follow symlinks")
        if not stat.S_ISREG(current.lstat().st_mode):
            raise ValueError("package inventory must name regular files")
    return files


def prepare(root, binary, database, output, notices):
    binary = regular_absolute(binary)
    database = regular_absolute(database)
    if not os.access(binary, os.X_OK):
        raise ValueError("selected executable must be executable")
    if not output.is_absolute():
        raise ValueError("output must be an absolute new directory")
    # Resolve the parent before reserving a fresh output; existing paths are errors.
    output = output.parent.resolve(strict=True) / output.name
    package_source = root / "plugins/foundry"
    files = package_files(package_source)
    if not notices.is_absolute() or notices.is_symlink() or not notices.is_dir():
        raise ValueError("select an absolute applicable-notices directory")
    notices = notices.resolve(strict=True)
    if not any(notices.iterdir()):
        raise ValueError("selected notices directory must not be empty")
    if notices == output or notices in output.parents:
        raise ValueError("output must be outside the notices directory")
    if output == root or root in output.parents:
        raise ValueError("output must be outside the source checkout")
    output.mkdir(mode=0o700)
    try:
        copy_content(root / "LICENSE", output / "LICENSE")
        copy_content(notices, output / "THIRD_PARTY_NOTICES")
        for relative in DISCOVERY:
            destination = output / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            copy_content(root / relative, destination)
        package = output / "plugins/foundry"
        package.mkdir(parents=True)
        for relative in files:
            destination = package / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            copy_content(package_source / relative, destination)
        bundled = package / "bin/f5y"
        bundled.parent.mkdir()
        digest = hashlib.sha256()
        # The selected file is copied, never executed by preparation.
        with binary.open("rb") as source, bundled.open("xb") as destination:
            while block := source.read(1024 * 1024):
                destination.write(block)
                digest.update(block)
        bundled.chmod(0o555)
        arguments = ["--database", str(database), "mcp"]
        write_json(package / "mcp.json", {
            "$schema": "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
            "mcpServers": {"foundry": {
                "type": "stdio", "command": "./bin/f5y", "args": arguments,
            }},
        })
        native = {"command": str(bundled), "args": arguments}
        write_json(package / ".mcp.json", {
            "mcpServers": {"foundry": {"type": "stdio", **native}},
        })
        write_json(package / ".cursor-plugin/mcp.json", {
            "mcpServers": {"foundry": native},
        })
        cursor_path = package / ".cursor-plugin/plugin.json"
        cursor = json.loads(cursor_path.read_text(encoding="utf-8"))
        cursor["mcpServers"] = "./.cursor-plugin/mcp.json"
        write_json(cursor_path, cursor)
        for relative in ("plugin.json", ".claude-plugin/plugin.json",
                         ".codex-plugin/plugin.json", ".cursor-plugin/plugin.json"):
            path = package / relative
            descriptor = json.loads(path.read_text(encoding="utf-8"))
            descriptor["description"] = "Foundry workflows with an explicitly configured local MCP server."
            if relative.startswith((".claude-plugin/", ".codex-plugin/")):
                descriptor["mcpServers"] = "./.mcp.json"
            write_json(path, descriptor)
        write_json(output / "prepared.json", {
            "version": 1,
            "executable_sha256": digest.hexdigest(),
            "database": str(database),
        })
    except BaseException:
        shutil.rmtree(output)
        raise
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--notices", type=Path, required=True,
                        help="Applicable notice packet for the selected executable; copied unchanged")
    args = parser.parse_args()
    try:
        result = prepare(ROOT, args.binary, args.database, args.output, args.notices)
    except (OSError, ValueError) as error:
        print(f"Client preparation failed: {error}", file=sys.stderr)
        return 1
    print(f"Prepared local client distribution: {result}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
