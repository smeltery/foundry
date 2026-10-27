#!/usr/bin/env python3
"""Run the same repository checks locally, in hooks, and in CI."""

import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
LICENSE_HASH = "5fce31b74a03790580196c1324b975d086c4281e9f9260123ae63ae09c63be11"


def run(*command):
    print("+ " + " ".join(command), flush=True)
    subprocess.run(command, cwd=ROOT, check=True)


def rust(fast):
    run("cargo", "fmt", "--all", "--check")
    run("cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
    if not fast:
        run("cargo", "test", "--workspace", "--locked", "--no-fail-fast")


def python_checks():
    suites = sorted(path.parent for path in (ROOT / "tests").glob("*/test_*.py"))
    for suite in sorted(set(suites)):
        run(sys.executable, "-m", "unittest", "discover", "-s", str(suite), "-v")
    run(sys.executable, "scripts/checks/catalog.py")
    run(sys.executable, "scripts/release/github_snapshot.py", "self-test")


def dependencies():
    run("cargo", "deny", "--locked", "check", "advisories", "bans", "sources")


def docs():
    run("bun", "run", "docs:test")
    run("bun", "run", "docs:check")


def adapter():
    run("bun", "run", "adapter:check")
    run("bun", "run", "adapter:test")


def integration():
    if os.name == "nt":
        raise ValueError("adapter process supervision requires a Unix platform")
    run("cargo", "build", "--locked", "--package", "f5y", "--package", "foundry-tools",
        "--bins", "--target-dir", str(ROOT / "target"))
    environment = dict(os.environ, FOUNDRY_TEST_BINARY=str(ROOT / "target/debug/f5y"),
                       FOUNDRY_TEST_TOOLHOST=str(ROOT / "target/debug/f5y-toolhost"))
    subprocess.run(
        ["bun", "test", "packages/adapter/test/rust-interop.test.ts"],
        cwd=ROOT, env=environment, check=True,
    )
    subprocess.run(
        ["cargo", "test", "--locked", "--package", "foundry-service", "--test", "tools",
         "real_toolhost_executes_a_granted_write_and_commits_receipt", "--", "--ignored", "--exact"],
        cwd=ROOT, env=environment, check=True,
    )
    subprocess.run(
        ["cargo", "test", "--locked", "--package", "f5y", "--test", "granted_execution",
         "--", "--ignored"],
        cwd=ROOT, env=environment, check=True,
    )


def mcp_sdk():
    if sys.version_info[:2] != (3, 13):
        raise ValueError("MCP SDK checks require the pinned Python 3.13 environment")
    environment = ROOT / ".flox/cache/mcp-sdk"
    interpreter = environment / "bin/python"
    if not interpreter.exists():
        run(sys.executable, "-m", "venv", str(environment))
    run(str(interpreter), "-m", "pip", "install", "--disable-pip-version-check",
        "--only-binary=:all:", "--require-hashes", "-r", "tests/mcp-sdk/requirements.txt")
    run("cargo", "build", "--locked", "--package", "f5y", "--target-dir", str(ROOT / "target"))
    run(str(interpreter), "tests/mcp-sdk/verify.py", "--binary", str(ROOT / "target/debug/f5y"),
        "--report", str(ROOT / ".flox/cache/mcp-sdk-result.json"))


def hygiene():
    if hashlib.sha256((ROOT / "LICENSE").read_bytes()).hexdigest() != LICENSE_HASH:
        raise ValueError("root license does not match the approved digest")
    run("git", "diff", "--check")
    run("git", "diff", "--cached", "--check")
    run(sys.executable, "scripts/checks/loc.py")
    for mode in ("tracked", "history"):
        run(sys.executable, "scripts/checks/naming.py", "--policy-json-env",
            "FOUNDRY_NAMING_POLICY", "--mode", mode)
    run("actionlint")
    run("shellcheck", "scripts/ci/browser-sandbox.sh")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--group", choices=("all", "rust", "dependencies", "python", "adapter", "integration", "mcp-sdk", "docs", "hygiene"), default="all")
    parser.add_argument("--fast", action="store_true", help="Skip Rust test execution; retain documentation validation")
    args = parser.parse_args()
    commands = {"rust": lambda: rust(args.fast), "dependencies": dependencies, "python": python_checks, "adapter": adapter, "integration": integration, "mcp-sdk": mcp_sdk, "docs": docs, "hygiene": hygiene}
    try:
        for name, check in commands.items():
            if args.group in ("all", name):
                check()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Check failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
