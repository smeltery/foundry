"""Pure release decisions over caller-verified snapshots; performs no I/O."""

from __future__ import annotations

from dataclasses import dataclass
import re

_SHA = re.compile(r"[0-9a-f]{40}\Z")
_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
_VERSION = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z")


class InvalidSnapshot(ValueError):
    """Input is malformed or conflicts with immutable release state."""


@dataclass(frozen=True)
class CI:
    sha: str
    conclusion: str
    repository: str
    workflow: str
    event: str = "push"
    branch: str = "main"


@dataclass(frozen=True)
class Tag:
    name: str
    sha: str


@dataclass(frozen=True)
class Asset:
    name: str
    sha256: str


@dataclass(frozen=True)
class Build:
    sha: str
    assets: tuple[Asset, ...]


@dataclass(frozen=True)
class Release:
    tag: str
    sha: str
    state: str
    assets: tuple[Asset, ...] = ()


@dataclass(frozen=True)
class Request:
    enabled: bool
    candidate_sha: str
    ci: CI
    repository: str
    workflow: str
    expected_assets: tuple[str, ...]
    tags: tuple[Tag, ...] = ()
    releases: tuple[Release, ...] = ()
    build: Build | None = None
    requested_tag: str | None = None


@dataclass(frozen=True)
class Plan:
    status: str
    sha: str
    tag: str | None = None
    actions: tuple[str, ...] = ()


def _sha(value: str) -> None:
    if not isinstance(value, str) or not _SHA.fullmatch(value):
        raise InvalidSnapshot("Expected a full lowercase commit SHA")


def _version(value: str) -> tuple[int, int, int]:
    match = _VERSION.fullmatch(value) if isinstance(value, str) else None
    if match is None:
        raise InvalidSnapshot("Expected a canonical stable version tag")
    return tuple(int(part) for part in match.groups())


def _names(names: tuple[str, ...]) -> set[str]:
    if any(not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", name)
           for name in names):
        raise InvalidSnapshot("Asset names must be plain file names")
    if len(names) != len(set(names)):
        raise InvalidSnapshot("Duplicate asset names")
    return set(names)


def _assets(assets: tuple[Asset, ...]) -> dict[str, str]:
    _names(tuple(asset.name for asset in assets))
    if any(not isinstance(asset.sha256, str) or not _DIGEST.fullmatch(asset.sha256)
           for asset in assets):
        raise InvalidSnapshot("Expected a SHA256 digest for every verified asset")
    return {asset.name: asset.sha256 for asset in assets}


def plan_release(request: Request) -> Plan:
    """Return the next safe phase, never perform it or imply API verification.

    The caller authenticates CI, peels tag SHAs, verifies artifact bytes, and
    supplies a complete stable-tag inventory under a publication lock. After
    each phase it must fetch fresh state and call again; plans are not locks.
    """
    _sha(request.candidate_sha)
    if type(request.enabled) is not bool:
        raise InvalidSnapshot("Enablement must be a boolean")
    if not request.enabled:
        return Plan("disabled", request.candidate_sha)
    ci = request.ci
    _sha(ci.sha)
    if not request.repository or not request.workflow:
        raise InvalidSnapshot("Expected repository and workflow identities")
    if (ci.repository != request.repository or ci.workflow != request.workflow
            or ci.event != "push" or ci.branch != "main"):
        return Plan("untrusted", request.candidate_sha)
    if ci.conclusion != "success":
        return Plan("ci_incomplete", request.candidate_sha)
    if ci.sha != request.candidate_sha:
        raise InvalidSnapshot("Candidate does not match the tested commit")
    expected = _names(request.expected_assets)
    if not expected:
        raise InvalidSnapshot("Expected asset inventory cannot be empty")
    tags: dict[str, str] = {}
    for tag in request.tags:
        _version(tag.name)
        _sha(tag.sha)
        if tag.name in tags:
            raise InvalidSnapshot("Duplicate tag snapshot")
        tags[tag.name] = tag.sha
    releases: dict[str, Release] = {}
    for release in request.releases:
        _version(release.tag)
        _sha(release.sha)
        _assets(release.assets)
        if release.state not in {"draft", "published"}:
            raise InvalidSnapshot("Invalid release state")
        if release.tag in releases:
            raise InvalidSnapshot("Duplicate release snapshot")
        if tags.get(release.tag) != release.sha:
            raise InvalidSnapshot("Release and tag commit conflict")
        releases[release.tag] = release
    candidates = [name for name, sha in tags.items() if sha == request.candidate_sha]
    if len(candidates) > 1:
        raise InvalidSnapshot("Multiple stable tags identify the candidate")
    if request.requested_tag is not None:
        _version(request.requested_tag)
        if tags.get(request.requested_tag) != request.candidate_sha:
            raise InvalidSnapshot("Requested recovery tag does not identify the candidate")
    if candidates:
        tag = candidates[0]
    else:
        major, minor, patch = max(map(_version, tags), default=(0, 0, 0))
        tag = f"v{major}.{minor}.{patch + 1}" if tags else "v0.1.0"
        return Plan("reserve", request.candidate_sha, tag, ("create_tag",))
    if request.build is None:
        return Plan("build", request.candidate_sha, tag, ("build_assets",))
    _sha(request.build.sha)
    if request.build.sha != request.candidate_sha:
        raise InvalidSnapshot("Build does not identify the tested commit")
    built = _assets(request.build.assets)
    if set(built) - expected:
        raise InvalidSnapshot("Unexpected build assets")
    if set(built) != expected:
        return Plan("build_incomplete", request.candidate_sha, tag, ("build_assets",))
    release = releases.get(tag)
    if release is None:
        return Plan("stage", request.candidate_sha, tag, ("create_draft", "upload_assets"))
    uploaded = _assets(release.assets)
    if any(name not in built or built[name] != digest for name, digest in uploaded.items()):
        raise InvalidSnapshot("Release assets conflict with verified build")
    if release.state == "published":
        if uploaded != built:
            raise InvalidSnapshot("Published release has incomplete assets")
        return Plan("complete", request.candidate_sha, tag)
    if uploaded != built:
        return Plan("stage", request.candidate_sha, tag, ("upload_assets",))
    return Plan("publish", request.candidate_sha, tag, ("publish_release",))
