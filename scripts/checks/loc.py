#!/usr/bin/env python3
"""Check physical source-line budgets without excluding comments or blank lines."""

import argparse
import json
from pathlib import Path
import subprocess
import sys


EXTENSIONS = {".rs", ".ts", ".tsx", ".js", ".mjs", ".py", ".sh", ".nix"}


def check(root, policy, paths):
    failures = []
    counts = {}
    exceptions = policy.get("files", {})
    for relative in sorted(set(paths)):
        path = root / relative
        if path.suffix not in EXTENSIONS or not path.is_file():
            continue
        if path.is_symlink():
            failures.append(f"{relative}: source symlinks need explicit review")
            continue
        count = len(path.read_text(encoding="utf-8").splitlines())
        counts[relative] = count
        if not any(relative.startswith(prefix) for prefix in policy.get("components", {})):
            failures.append(f"{relative}: no component budget is configured")
        exception = exceptions.get(relative)
        if exception and not all(exception.get(key) for key in ("owner", "rationale")):
            failures.append(f"{relative}: exception lacks owner or rationale")
        limit = exception["limit"] if exception else policy["default_file_lines"]
        if count > limit:
            failures.append(f"{relative}: {count} lines exceeds {limit}")
    for component, limit in policy.get("components", {}).items():
        total = sum(count for path, count in counts.items() if path.startswith(component))
        if total > limit:
            failures.append(f"{component}: {total} lines exceeds component budget {limit}")
    return counts, failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--policy", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    policy_path = args.policy or root / "scripts/checks/loc-budgets.json"
    try:
        policy = json.loads(policy_path.read_text())
        result = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            capture_output=True, check=True,
        )
        paths = result.stdout.decode().rstrip("\0").split("\0") if result.stdout else []
        counts, failures = check(root, policy, paths)
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"LOC check failed: {error}", file=sys.stderr)
        return 2
    for failure in failures:
        print(failure, file=sys.stderr)
    print(f"Checked {len(counts)} source files: {sum(counts.values())} physical lines")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
