"""Deterministic two-executable packages and strict byte inventory verification."""

import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile

from planner import Asset, Build, InvalidSnapshot

BINARIES = ("f5y", "f5y-toolhost")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def manifest(sha, tag, target, payloads):
    return json.dumps({"sha": sha, "tag": tag, "target": target,
                       "binaries": {name: {"version": tag[1:], "sha256": digest(payloads[name])}
                                    for name in BINARIES}}, sort_keys=True).encode()


def pack(args, notices, command):
    payloads = {}
    for name in BINARIES:
        path = Path("target") / args.target / "release" / name
        if path.is_symlink() or not path.is_file() or not path.stat().st_mode & 0o111:
            raise InvalidSnapshot("Required executable is missing or invalid")
        data = path.read_bytes()
        if not data:
            raise InvalidSnapshot("Empty executable")
        if command(str(path), "--version") != f"{name} {args.tag[1:]}":
            raise InvalidSnapshot("Executable version must match release tag")
        if path.read_bytes() != data:
            raise InvalidSnapshot("Executable changed during version verification")
        payloads[name] = data
    output = Path(args.directory)
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"f5y-{args.tag}-{args.target}.tar.gz"
    entries = [(name, payloads[name], 0o755) for name in BINARIES]
    entries += [("build.json", manifest(args.sha, args.tag, args.target, payloads), 0o644)]
    entries += [(name, data, 0o644) for name, data in sorted(notices.items())]
    with archive.open("wb") as stream, gzip.GzipFile(fileobj=stream, mode="wb", mtime=0, filename="") as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as bundle:
            for name, data, mode in entries:
                info = tarfile.TarInfo(name)
                info.size, info.mode = len(data), mode
                bundle.addfile(info, io.BytesIO(data))
    archive.with_name(archive.name + ".sha256").write_text(f"{digest(archive.read_bytes())}  {archive.name}\n")


def verify(directory, tag, sha, targets, expected_assets, notices):
    root = Path(directory)
    if set(path.name for path in root.iterdir()) != set(expected_assets):
        raise InvalidSnapshot("Artifact inventory mismatch")
    assets = []
    for target in targets:
        name = f"f5y-{tag}-{target}.tar.gz"
        archive = root / name
        if archive.is_symlink() or not archive.is_file():
            raise InvalidSnapshot("Archive must be a regular file")
        checksum = root / (name + ".sha256")
        archive_digest = digest(archive.read_bytes())
        if (checksum.is_symlink() or not checksum.is_file()
                or checksum.read_text() != f"{archive_digest}  {name}\n"):
            raise InvalidSnapshot("Archive checksum mismatch")
        with tarfile.open(archive, "r:gz") as bundle:
            members = bundle.getmembers()
            expected_members = {*BINARIES, "build.json", *notices}
            if (len(members) != len(expected_members) or {item.name for item in members} != expected_members
                    or any(not item.isfile() for item in members)):
                raise InvalidSnapshot("Unsafe or incomplete archive")
            for path, expected_bytes in notices.items():
                if bundle.extractfile(path).read() != expected_bytes:
                    raise InvalidSnapshot("Archived notice or inventory bytes differ from tested commit")
            payloads = {}
            for binary in BINARIES:
                member = bundle.getmember(binary)
                if member.size == 0 or member.mode != 0o755:
                    raise InvalidSnapshot("Empty or invalid executable payload")
                payloads[binary] = bundle.extractfile(binary).read()
            # Exact canonical bytes also reject duplicate/extra manifest fields.
            if bundle.extractfile("build.json").read() != manifest(sha, tag, target, payloads):
                raise InvalidSnapshot("Executable inventory or archive provenance mismatch")
        assets.extend((Asset(name, archive_digest), Asset(checksum.name, digest(checksum.read_bytes()))))
    return Build(sha, tuple(assets))
