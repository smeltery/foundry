#!/usr/bin/env python3
"""Verify action-installed tools and install digest-pinned Linux CI utilities."""

import argparse
import hashlib
import io
import os
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import urllib.request


GROUPS = ("rust", "dependencies", "python", "adapter", "integration", "mcp-sdk", "docs", "hygiene")
RUST_GROUPS = {"rust", "dependencies", "integration", "mcp-sdk"}
BUN_GROUPS = {"adapter", "integration", "docs"}
MAX_ARCHIVE = 64 * 1024 * 1024
# SHA256 digests verified against the publishers' GitHub release assets.
ASSETS = {
    "cargo-deny": (
        "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.19.9/"
        "cargo-deny-0.19.9-x86_64-unknown-linux-musl.tar.gz",
        "f1f8eedc2a3ac297c540873f93785d4104b102c0079506b2a6b3221b7ec956af",
        "cargo-deny-0.19.9-x86_64-unknown-linux-musl/cargo-deny",
    ),
    "actionlint": (
        "https://github.com/rhysd/actionlint/releases/download/v1.7.12/"
        "actionlint_1.7.12_linux_amd64.tar.gz",
        "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8",
        "actionlint",
    ),
    "shellcheck": (
        "https://github.com/koalaman/shellcheck/releases/download/v0.11.0/"
        "shellcheck-v0.11.0.linux.x86_64.tar.gz",
        "b7af85e41cc99489dcc21d66c6d5f3685138f06d34651e6d34b42ec6d54fe6f6",
        "shellcheck-v0.11.0/shellcheck",
    ),
}


def output(*command):
    result = subprocess.check_output(command, text=True).strip()
    print(f"{' '.join(command)}: {result}", flush=True)
    return result


def require_version(command, expected):
    if expected not in output(*command).split():
        raise ValueError(f"Unexpected version for {command[0]}; expected {expected}")


def executable_payload(data, digest, member):
    if len(data) > MAX_ARCHIVE or hashlib.sha256(data).hexdigest() != digest:
        raise ValueError("Tool archive size or SHA256 mismatch")
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        matches = [entry for entry in archive.getmembers() if entry.name == member]
        if len(matches) != 1 or not matches[0].isfile():
            raise ValueError("Expected exactly one regular executable")
        entry = matches[0]
        if not 0 < entry.size <= MAX_ARCHIVE:
            raise ValueError("Executable size exceeds bound")
        # Read only the known member, never extract archive-controlled paths.
        with archive.extractfile(entry) as stream:
            data = stream.read(MAX_ARCHIVE + 1)
        if len(data) != entry.size:
            raise ValueError("Truncated executable")
        return data


def install(name, directory):
    url, digest, member = ASSETS[name]
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read(MAX_ARCHIVE + 1)
    payload = executable_payload(data, digest, member)
    target = directory / name
    target.write_bytes(payload)
    target.chmod(0o755)


def prepend_path(directory):
    value = str(directory)
    if "\n" in value or "\r" in value:
        raise ValueError("Invalid tool directory")
    os.environ["PATH"] = value + os.pathsep + os.environ["PATH"]
    with Path(os.environ["GITHUB_PATH"]).open("a") as stream:
        stream.write(value + "\n")


def bootstrap(group):
    require_version((sys.executable, "--version"), "3.13.13")
    output("git", "--version")
    if group in RUST_GROUPS:
        cargo = Path(output("rustup", "which", "--toolchain", "1.95.0", "cargo"))
        prepend_path(cargo.parent)
        require_version(("rustc", "--version"), "1.95.0")
        require_version(("cargo", "--version"), "1.95.0")
        output("pkg-config", "--version")
    if group in BUN_GROUPS:
        require_version(("bun", "--version"), "1.3.11")
    if group in {"docs", "adapter"}:
        require_version(("node", "--version"), "v22.23.1")
    utilities = {"dependencies": ["cargo-deny"], "hygiene": ["actionlint", "shellcheck"]}
    if group not in utilities:
        return
    if (platform.system(), platform.machine()) != ("Linux", "x86_64"):
        raise ValueError("Pinned utility archives require Linux x86_64")
    directory = Path(os.environ["RUNNER_TEMP"]) / "foundry-ci-bin"
    directory.mkdir(exist_ok=True)
    for name in utilities[group]:
        install(name, directory)
    prepend_path(directory)
    if group == "dependencies":
        require_version(("cargo-deny", "--version"), "0.19.9")
    else:
        require_version(("actionlint", "-version"), "1.7.12")
        require_version(("shellcheck", "--version"), "0.11.0")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--group", choices=GROUPS, required=True)
    try:
        bootstrap(parser.parse_args().group)
    except (OSError, ValueError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"CI bootstrap failed: {error}", file=sys.stderr)
        sys.exit(1)
