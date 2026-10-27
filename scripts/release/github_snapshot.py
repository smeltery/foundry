#!/usr/bin/env python3
"""Authenticated release snapshots, guarded execution, and offline checks."""

from __future__ import annotations

import argparse
import artifacts
from dataclasses import replace
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from planner import Asset, Build, CI, InvalidSnapshot, Release, Request, Tag, plan_release

TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "aarch64-apple-darwin")
STABLE = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z")


def command(*args, binary=False):
    return subprocess.check_output(args, text=not binary).strip() if not binary else subprocess.check_output(args)


def api(path, payload=None, pages=False):
    args = ["gh", "api", path]
    if pages:
        args += ["--paginate", "--slurp"]
    if payload is None:
        result = command(*args)
    else:
        result = subprocess.check_output(
            [*args, "--method", "POST", "--input", "-"], input=json.dumps(payload), text=True)
    value = json.loads(result)
    return [item for page in value for item in page] if pages else value


def trusted_ci(run, workflow_id, repository, sha, attempt):
    if (run.get("head_repository", {}).get("full_name") != repository
            or run.get("repository", {}).get("full_name") != repository
            or run.get("workflow_id") != workflow_id or run.get("run_attempt") != attempt
            or run.get("status") != "completed"):
        raise InvalidSnapshot("CI provenance mismatch")
    ci = CI(run["head_sha"], run["conclusion"], repository, ".github/workflows/ci.yml",
            run["event"], run["head_branch"])
    probe = Request(True, sha, ci, repository, ci.workflow, ("probe",))
    if plan_release(probe).status != "reserve":
        raise InvalidSnapshot("CI is not eligible for release")
    return ci


def inventory(repository):
    tags = []
    for item in api(f"repos/{repository}/tags?per_page=100", pages=True):
        name = item["name"]
        if STABLE.fullmatch(name):
            # The commits endpoint dereferences annotated tags as well.
            sha = api(f"repos/{repository}/commits/{name}")["sha"]
            tags.append(Tag(name, sha))
    return tuple(tags)


def expected_assets(tag):
    return tuple(f"f5y-{tag}-{target}.{suffix}" for target in TARGETS
                 for suffix in ("tar.gz", "tar.gz.sha256"))


def notice_inventory():
    paths = command("git", "ls-tree", "-r", "--name-only", "HEAD", "LICENSE", "licenses", "apps/cli").splitlines()
    paths = [path for path in paths if path == "LICENSE" or path.startswith("licenses/")
             or re.fullmatch(r"(?:LICENSE|NOTICE)(?:[.-].*)?", Path(path).name, re.IGNORECASE)]
    if not {"LICENSE", "licenses/cli-template.txt"}.issubset(paths):
        raise InvalidSnapshot("Required committed notices are missing")
    files = {path: command("git", "show", f"HEAD:{path}", binary=True) for path in paths}
    if any(not data for data in files.values()):
        raise InvalidSnapshot("Empty notice file")
    manifest = {"files": {path: hashlib.sha256(data).hexdigest() for path, data in files.items()},
                "lock_sha256": hashlib.sha256(command("git", "show", "HEAD:Cargo.lock", binary=True)).hexdigest(),
                "dependency_coverage": "pending"}
    return {**files, "notices.json": json.dumps(manifest, sort_keys=True).encode()}


def publication_checks(directory):
    subprocess.run([sys.executable, "scripts/checks/naming.py", "--mode", "release", "--release", directory,
                    "--policy-json-env", "FOUNDRY_NAMING_POLICY"], check=True)
    # A text-only scan cannot authorize binary distribution or prove license coverage.
    raise InvalidSnapshot("Dependency notice coverage is pending; publication remains blocked")


def workflow_revision(sha):
    actual = os.environ.get("FOUNDRY_WORKFLOW_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", actual) or actual != sha:
        raise InvalidSnapshot("Workflow revision must equal tested candidate")


def snapshot(args):
    workflow_revision(args.sha)
    repository = os.environ["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise InvalidSnapshot("Invalid repository identity")
    workflow = api(f"repos/{repository}/actions/workflows/ci.yml")
    run = api(f"repos/{repository}/actions/runs/{args.run_id}/attempts/{args.attempt}")
    ci = trusted_ci(run, workflow["id"], repository, args.sha, args.attempt)
    current = api(f"repos/{repository}/actions/runs/{args.run_id}")
    if trusted_ci(current, workflow["id"], repository, args.sha, args.attempt) != ci:
        raise InvalidSnapshot("Current CI differs from the trusted attempt")
    if command("git", "rev-parse", "HEAD") != args.sha:
        raise InvalidSnapshot("Checkout must match tested commit")
    main_sha = api(f"repos/{repository}/commits/main")["sha"]
    comparison = api(f"repos/{repository}/compare/{args.sha}...{main_sha}")
    if comparison["status"] not in {"identical", "ahead"}:
        raise InvalidSnapshot("Candidate is not on main history")
    tags = inventory(repository)
    request = Request(True, args.sha, ci, repository, ci.workflow, ("probe",),
                      tags=tags, requested_tag=args.tag or None)
    result = plan_release(request)
    if result.status == "reserve" and main_sha != args.sha:
        raise InvalidSnapshot("Obsolete unreserved candidate; await current main CI")
    if args.tag and args.tag != result.tag:
        raise InvalidSnapshot("Requested tag conflicts with candidate")
    return replace(request, expected_assets=expected_assets(result.tag)), result


def reserve(args):
    for _ in range(3):
        request, result = snapshot(args)
        if result.status != "reserve":
            break
        try:
            api(f"repos/{request.repository}/git/refs",
                {"ref": f"refs/tags/{result.tag}", "sha": result.sha})
        except subprocess.CalledProcessError:
            continue
        break
    request, result = snapshot(args)
    if result.status == "reserve":
        raise InvalidSnapshot("Tag reservation did not complete")
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"tag={result.tag}\n")
    print(json.dumps({"tag": result.tag, "sha": result.sha, "status": result.status}))


def pack(args):
    workflow_revision(args.sha)
    if args.target not in TARGETS or not STABLE.fullmatch(args.tag):
        raise InvalidSnapshot("Invalid package target or tag")
    if command("git", "rev-parse", "HEAD") != args.sha:
        raise InvalidSnapshot("Package checkout mismatch")
    artifacts.pack(args, notice_inventory(), command)


def verified_build(directory, tag, sha):
    return artifacts.verify(directory, tag, sha, TARGETS, expected_assets(tag), notice_inventory())


def remote_release(repository, tag, sha):
    entries = api(f"repos/{repository}/releases?per_page=100", pages=True)
    version = tuple(map(int, STABLE.fullmatch(tag).groups()))
    if any(not entry["draft"] and STABLE.fullmatch(entry["tag_name"])
           and tuple(map(int, STABLE.fullmatch(entry["tag_name"]).groups())) > version
           for entry in entries):
        raise InvalidSnapshot("A newer stable release is already published")
    matches = [entry for entry in entries if entry["tag_name"] == tag]
    if len(matches) > 1:
        raise InvalidSnapshot("Duplicate release objects")
    if not matches:
        return None, None
    entry = matches[0]
    if entry.get("body") != f"Foundry build {sha}" or entry.get("prerelease"):
        raise InvalidSnapshot("Release ownership mismatch")
    assets = []
    for item in api(f"repos/{repository}/releases/{entry['id']}/assets?per_page=100", pages=True):
        data = command("gh", "api", f"repos/{repository}/releases/assets/{item['id']}",
                       "-H", "Accept: application/octet-stream", binary=True)
        assets.append(Asset(item["name"], hashlib.sha256(data).hexdigest()))
    return entry["id"], Release(tag, sha, "draft" if entry["draft"] else "published", tuple(assets))


def publication_state(args, build):
    request, _ = snapshot(args)
    release_id, release = remote_release(request.repository, args.tag, args.sha)
    return (replace(request, build=build, requested_tag=args.tag,
                    releases=(release,) if release else ()), release_id, release)


def publish(args):
    publication_checks(args.directory)
    build = verified_build(args.directory, args.tag, args.sha)
    for _ in range(4):
        request, release_id, release = publication_state(args, build)
        result = plan_release(request)
        if result.status == "complete":
            print("Published release and all asset bytes verified")
            return
        if result.status == "stage":
            if release is None:
                api(f"repos/{request.repository}/releases", {"tag_name": args.tag,
                    "target_commitish": args.sha, "name": args.tag, "draft": True,
                    "body": f"Foundry build {args.sha}"})
            uploaded = {asset.name for asset in release.assets} if release else set()
            missing = [str(Path(args.directory) / name) for name in request.expected_assets if name not in uploaded]
            if missing:
                command("gh", "release", "upload", args.tag, *missing, "--repo", request.repository)
        elif result.status == "publish":
            # Re-read tag, current CI, release identity and actual remote asset bytes.
            request, release_id, _ = publication_state(args, build)
            fresh = plan_release(request)
            if fresh.status == "complete":
                print("Published release and all asset bytes verified")
                return
            if fresh.status != "publish":
                raise InvalidSnapshot("Release changed before publication")
            command("gh", "api", f"repos/{request.repository}/releases/{release_id}",
                    "--method", "PATCH", "-F", "draft=false", "-f", "make_latest=true")
        else:
            raise InvalidSnapshot("Release is not ready for publication")
    raise InvalidSnapshot("Release verification did not converge")


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.sha = "a" * 40
        self.run = {"head_sha": self.sha, "conclusion": "success", "event": "push", "head_branch": "main",
                    "head_repository": {"full_name": "smeltery/foundry"},
                    "repository": {"full_name": "smeltery/foundry"}, "workflow_id": 7,
                    "run_attempt": 1, "status": "completed"}

    def test_trusted(self):
        self.assertEqual(trusted_ci(self.run, 7, "smeltery/foundry", self.sha, 1).sha, self.sha)

    def test_fail_closed(self):
        for key, value in (("head_sha", "b" * 40), ("workflow_id", 8), ("run_attempt", 2),
                           ("event", "pull_request"), ("conclusion", "failure"), ("status", "queued"),
                           ("head_repository", {"full_name": "fork/foundry"}), ("head_branch", "topic")):
            with self.subTest(key=key), self.assertRaises(InvalidSnapshot):
                trusted_ci(dict(self.run, **{key: value}), 7, "smeltery/foundry", self.sha, 1)

    def test_incomplete_artifacts(self):
        with tempfile.TemporaryDirectory() as directory, patch(__name__ + ".notice_inventory", return_value={}), \
                self.assertRaises(InvalidSnapshot):
            verified_build(directory, "v0.1.0", self.sha)

    def test_notice_inventory_uses_committed_bytes(self):
        with patch(__name__ + ".command", side_effect=["LICENSE\nlicenses/cli-template.txt", b"root", b"app", b"lock"]):
            notices = notice_inventory()
        self.assertEqual(notices["LICENSE"], b"root")
        self.assertEqual(json.loads(notices["notices.json"])["files"]["licenses/cli-template.txt"],
                         hashlib.sha256(b"app").hexdigest())
        with patch(__name__ + ".command", return_value="LICENSE"), self.assertRaises(InvalidSnapshot):
            notice_inventory()

    def test_disabled_cli_performs_no_api_calls(self):
        with patch.dict(os.environ, {"FOUNDRY_RELEASE_ENABLED": "false"}), \
                patch("sys.argv", ["snapshot", "reserve", "--execute"]), \
                patch(__name__ + ".api") as remote:
            with self.assertRaises(InvalidSnapshot):
                main()
            remote.assert_not_called()

    def test_tag_collision_refetches_before_recovery(self):
        ci = trusted_ci(self.run, 7, "smeltery/foundry", self.sha, 1)
        request = Request(True, self.sha, ci, ci.repository, ci.workflow, ("asset",))
        reserved = replace(request, tags=(Tag("v0.1.0", self.sha),))
        states = [(request, plan_release(request)), (reserved, plan_release(reserved)),
                  (reserved, plan_release(reserved))]
        with tempfile.TemporaryDirectory() as directory, \
                patch.dict(os.environ, {"GITHUB_OUTPUT": str(Path(directory) / "output")}), \
                patch(__name__ + ".snapshot", side_effect=states) as fetch, \
                patch(__name__ + ".api", side_effect=subprocess.CalledProcessError(1, "gh")) as remote:
            reserve(argparse.Namespace())
            self.assertEqual(fetch.call_count, 3)
            self.assertEqual(remote.call_count, 1)
            self.assertEqual((Path(directory) / "output").read_text(), "tag=v0.1.0\n")

    def test_partial_draft_retry_publishes_once(self):
        ci = trusted_ci(self.run, 7, "smeltery/foundry", self.sha, 1)
        assets = (Asset("asset", "c" * 64),)
        request = Request(True, self.sha, ci, ci.repository, ci.workflow,
                          ("asset",), tags=(Tag("v0.1.0", self.sha),))
        draft = Release("v0.1.0", self.sha, "draft")
        states = [(9, draft), (9, replace(draft, assets=assets)),
                  (9, replace(draft, assets=assets)),
                  (9, replace(draft, state="published", assets=assets))]
        args = argparse.Namespace(tag="v0.1.0", sha=self.sha, directory="artifacts")
        with patch(__name__ + ".snapshot", return_value=(request, plan_release(request))), \
                patch(__name__ + ".publication_checks"), \
                patch(__name__ + ".verified_build", return_value=Build(self.sha, assets)), \
                patch(__name__ + ".remote_release", side_effect=states), \
                patch(__name__ + ".command") as execute:
            publish(args)
            self.assertEqual(execute.call_count, 2)
            self.assertIn("upload", execute.call_args_list[0].args)
            self.assertIn("PATCH", execute.call_args_list[1].args)

    def test_newer_release_prevents_latest_rollback(self):
        with patch(__name__ + ".api", return_value=[{"tag_name": "v0.1.1", "draft": False}]), \
                self.assertRaises(InvalidSnapshot):
            remote_release("smeltery/foundry", "v0.1.0", self.sha)

    def test_verified_artifacts_and_corruption(self):
        notices = {"LICENSE": b"root notice", "licenses/cli-template.txt": b"app notice", "notices.json": b"{}"}
        with tempfile.TemporaryDirectory() as directory, \
                patch(__name__ + ".notice_inventory", return_value=notices) as inventory_mock:
            for target in TARGETS:
                archive = Path(directory) / f"f5y-v0.1.0-{target}.tar.gz"
                with tarfile.open(archive, "w:gz") as bundle:
                    for name, data in (*notices.items(), ("f5y", b"binary"), ("f5y-toolhost", b"helper"),
                                       ("build.json", artifacts.manifest(self.sha, "v0.1.0", target,
                                            {"f5y": b"binary", "f5y-toolhost": b"helper"}))):
                        info = tarfile.TarInfo(name)
                        info.size = len(data)
                        info.mode = 0o755 if name in artifacts.BINARIES else 0o644
                        bundle.addfile(info, io.BytesIO(data))
                checksum = archive.with_name(archive.name + ".sha256")
                checksum.write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
            self.assertEqual(len(verified_build(directory, "v0.1.0", self.sha).assets), 6)
            for path in notices:
                inventory_mock.return_value = {**notices, path: b"changed"}
                with self.assertRaises(InvalidSnapshot):
                    verified_build(directory, "v0.1.0", self.sha)
            inventory_mock.return_value = notices
            with self.assertRaises(InvalidSnapshot):
                verified_build(directory, "v0.1.0", "b" * 40)
            checksum.write_text("invalid\n")
            with self.assertRaises(InvalidSnapshot):
                verified_build(directory, "v0.1.0", self.sha)

    def test_publication_checks_fail_closed(self):
        for failure in (None, subprocess.CalledProcessError(2, "scanner")):
            with patch(__name__ + ".subprocess.run", side_effect=failure), \
                    patch(__name__ + ".snapshot") as remote, \
                    self.assertRaises((InvalidSnapshot, subprocess.CalledProcessError)):
                publish(argparse.Namespace(directory="artifacts"))
            remote.assert_not_called()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("reserve", "validate", "pack", "publish", "self-test"))
    parser.add_argument("--sha", default="")
    parser.add_argument("--tag", default="")
    parser.add_argument("--run-id", type=int)
    parser.add_argument("--attempt", type=int)
    parser.add_argument("--target")
    parser.add_argument("--directory", default="release-assets")
    parser.add_argument("--execute", action="store_true")
    args = parser.parse_args()
    if args.mode == "self-test":
        result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(SnapshotTests))
        return 0 if result.wasSuccessful() else 1
    if os.environ.get("FOUNDRY_RELEASE_ENABLED") != "true":
        raise InvalidSnapshot("Release execution is disabled")
    if args.mode in {"reserve", "publish"} and not args.execute:
        raise InvalidSnapshot("Remote writes require explicit execution")
    if args.mode == "reserve":
        reserve(args)
    elif args.mode == "validate":
        _, result = snapshot(args)
        if not args.tag or result.status == "reserve":
            raise InvalidSnapshot("Expected reserved tag")
    elif args.mode == "pack":
        pack(args)
    else:
        publish(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
