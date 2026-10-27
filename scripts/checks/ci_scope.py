#!/usr/bin/env python3
"""Select conservative PR check groups; pushes and manual runs verify everything."""

import json
import os
from pathlib import PurePosixPath
import re
import subprocess

GROUPS = ("rust", "dependencies", "python", "adapter", "integration", "mcp-sdk", "docs", "hygiene")
CRATES = {"contracts", "store", "runtime", "mcp", "service", "network", "connectors", "tools"}
SHARED = (".github/", ".flox/", "scripts/", "tests/", "vendor/", "crates/contracts/")


def select_groups(event, paths):
    if event != "pull_request" or not paths:
        return GROUPS
    selected = {"docs", "hygiene"}
    for path in paths:
        parts = PurePosixPath(path).parts
        if (not parts or path.startswith("/") or ".." in parts or "\\" in path
                or "/" not in path or path.startswith(SHARED)
                or parts[-1] in {"Cargo.toml", "Cargo.lock", "package.json", "bun.lock"}):
            return GROUPS
        if path.startswith("docs/"):
            continue
        if path.startswith("apps/cli/") or (len(parts) > 2 and parts[0] == "crates" and parts[1] in CRATES):
            selected.update(("rust", "integration", "mcp-sdk"))
        elif path.startswith("packages/adapter/"):
            selected.update(("rust", "adapter", "integration"))
        else:
            return GROUPS
    return tuple(group for group in GROUPS if group in selected)


def matrix(groups):
    jobs = [{"group": group, "runner": "ubuntu-24.04"} for group in groups]
    if "integration" in groups:
        jobs.extend({"group": "integration", "runner": runner}
                    for runner in ("ubuntu-24.04-arm", "macos-15"))
    return {"include": jobs}


def changed_paths(base, head, *, cwd=None):
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (base, head)):
        raise ValueError("PR comparison requires complete commit IDs")
    # Disabled rename detection exposes both removed and added paths for scope selection.
    data = subprocess.check_output([
        "git", "diff", "--name-only", "--no-renames", "-z", f"{base}...{head}", "--",
    ], cwd=cwd)
    return data.decode("utf-8").rstrip("\0").split("\0") if data else []


def main():
    event = os.environ["CI_EVENT"]
    paths = changed_paths(os.environ["CI_BASE"], os.environ["CI_HEAD"]) if event == "pull_request" else []
    groups = select_groups(event, paths)
    encoded = json.dumps(matrix(groups), separators=(",", ":"))
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"matrix={encoded}\n")
    print(f"Selected {len(matrix(groups)['include'])} jobs for {len(paths)} changed paths: {', '.join(groups)}")


if __name__ == "__main__":
    main()
