"""Archive-level notice parity plus naming and executable scans over extracted release members."""

import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile

from artifacts import BINARIES
import notices
from planner import InvalidSnapshot

CHECKS = Path(__file__).resolve().parents[1] / "checks"
SECRET_POLICY = ("--policy-json-env", "FOUNDRY_NAMING_POLICY")
# Release executables exceed the scanners' source-file defaults; explicit bounds still apply.
LIMITS = {"naming.py": ("--max-bytes", str(512 << 20), "--max-total-bytes", str(2 << 30)),
          "binary.py": ("--max-bytes", str(512 << 20), "--max-records", str(1 << 24))}
DEFERRED = {"binary_requires_external_inspection", "file_size_limit"}


def check_notices(files, committed, lock_bytes):
    """Archived licenses/** and notices.json must equal the committed bundle and describe each other."""
    if {name: files[name] for name in committed} != committed:
        raise InvalidSnapshot("Archived notices differ from the committed bundle")
    texts = {name: data for name, data in committed.items()
             if name.startswith(notices.BUNDLE_ROOT + "/") and name != notices.BUNDLE_MANIFEST}
    try:
        bundle = notices.verify_bundle(committed[notices.BUNDLE_MANIFEST], texts, lock_bytes)
        summary = json.loads(committed["notices.json"])
    except (KeyError, ValueError, TypeError, AttributeError) as error:
        raise InvalidSnapshot(f"Archived dependency notices lack bundle manifest parity: {error}") from error
    hashes = {name: notices.digest(data) for name, data in committed.items() if name != "notices.json"}
    if summary != {"files": hashes, "lock_sha256": bundle["lock_sha256"], "graph_sha256": bundle["graph_sha256"]}:
        raise InvalidSnapshot("Archived notices.json does not describe the archived notices")


def scan(script, policy, *arguments):
    result = subprocess.run([sys.executable, str(CHECKS / script), *arguments, *policy, *LIMITS[script]],
                            capture_output=True, text=True, timeout=600)
    try:
        return result.returncode, json.loads(result.stdout)
    except ValueError as error:
        raise InvalidSnapshot(f"{script} produced no report") from error


def audit(directory, tag, target, committed, lock_bytes, policy=SECRET_POLICY):
    """Return executable formats; an exact member inventory keeps extraction to known safe paths."""
    with tarfile.open(Path(directory) / f"f5y-{tag}-{target}.tar.gz", "r:gz") as bundle:
        items = bundle.getmembers()
        names = [item.name for item in items]
        if (len(set(names)) != len(names) or set(names) != {*BINARIES, "build.json", *committed}
                or any(not item.isfile() for item in items)):
            raise InvalidSnapshot("Archive members differ from the release inventory")
        files = {item.name: bundle.extractfile(item).read() for item in items}
    check_notices(files, committed, lock_bytes)
    with tempfile.TemporaryDirectory() as temporary:
        for name, data in files.items():
            Path(temporary, name).parent.mkdir(parents=True, exist_ok=True)
            Path(temporary, name).write_bytes(data)
        _, report = scan("naming.py", policy, "--mode", "release", "--release", temporary)
        if report.get("status") == "violations":
            raise InvalidSnapshot("Naming scan found policy matches in archive members")
        # Only executables may defer text inspection, and binary.py must then cover them.
        if report.get("errors") or any(item["path"] not in BINARIES or item["code"] not in DEFERRED
                                       for item in report.get("uninspected", [])):
            raise InvalidSnapshot("Naming scan of archive members was incomplete")
        formats = {}
        for name in BINARIES:
            code, result = scan("binary.py", policy, str(Path(temporary, name)))
            if result.get("findings"):
                raise InvalidSnapshot(f"Binary scan found policy matches in {name}")
            if code or result.get("status") != "evidence_only":
                raise InvalidSnapshot(f"Binary scan of {name} was incomplete")
            formats[name] = result["format"]
    return formats
