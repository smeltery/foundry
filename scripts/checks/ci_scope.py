#!/usr/bin/env python3
"""Select conservative PR check groups; pushes and manual runs verify everything."""

import json
import os
from pathlib import PurePosixPath
import re
import subprocess

GROUPS = ("rust", "dependencies", "python", "adapter", "integration", "mcp-sdk", "docs", "hygiene")
CRATES = {"contracts", "store", "runtime", "mcp", "service", "network", "connectors", "tools",
          "identity", "providers", "budget"}
SHARED = (".github/", ".flox/", "scripts/", "tests/", "vendor/", "crates/contracts/")
# Groups that finish in a minute or two share one runner instead of each paying for setup.
DEDICATED = ("rust", "integration")
BUN_GROUPS = {"adapter", "integration", "docs"}
ACTIVATION = (("x86_64-linux", "ubuntu-24.04"), ("aarch64-linux", "ubuntu-24.04-arm"),
              ("aarch64-darwin", "macos-15"), ("x86_64-darwin", "macos-15-intel"))
# Intel macOS runners are the slowest and scarcest; pull requests rely on main to cover them.
MAIN_ONLY_SYSTEMS = {"x86_64-darwin"}


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
        elif path.startswith("packages/provider-local/"):
            selected.add("adapter")
        else:
            return GROUPS
    return tuple(group for group in GROUPS if group in selected)


def job(name, checks, runner):
    return {"group": name, "checks": " ".join(checks), "runner": runner,
            "bun": any(check in BUN_GROUPS for check in checks), "browser": "docs" in checks}


def matrix(groups):
    jobs = [job(group, (group,), "ubuntu-24.04") for group in groups if group in DEDICATED]
    quick = tuple(group for group in groups if group not in DEDICATED)
    if quick:
        jobs.append(job("quick", quick, "ubuntu-24.04"))
    if "integration" in groups:
        jobs.extend(job("integration", ("integration",), runner)
                    for runner in ("ubuntu-24.04-arm", "macos-15"))
    return {"include": jobs}


def activation_matrix(event, checks):
    """Attach each system's activation to an integration job on its runner, if one runs.

    The host job then sets up the toolchain once for both; other systems keep their own job.
    """
    hosts = {job["runner"]: job for job in checks["include"] if job["group"] == "integration"}
    standalone = []
    for system, runner in ACTIVATION:
        if event == "pull_request" and system in MAIN_ONLY_SYSTEMS:
            continue
        if runner in hosts:
            hosts[runner]["system"] = system
        else:
            standalone.append({"system": system, "runner": runner})
    return {"include": standalone}


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
    checks = matrix(groups)
    activation = activation_matrix(event, checks)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"matrix={json.dumps(checks, separators=(',', ':'))}\n")
        output.write(f"activation={json.dumps(activation, separators=(',', ':'))}\n")
    print(f"Selected {len(checks['include'])} check and {len(activation['include'])} activation jobs "
          f"for {len(paths)} changed paths: {', '.join(groups)}")


if __name__ == "__main__":
    main()
