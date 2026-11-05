#!/usr/bin/env python3
"""Run a pinned campaign manifest against the fake or stub local provider.

Each task fixture runs once per seed and repeat. The run writes a resolved
`manifest.json` and one `outcomes.jsonl` record per call. Outcomes are recorded
without grading: no record scores, ranks or judges a reply, and nothing here
establishes model or benchmark equivalence.
"""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import endpoint  # noqa: E402

SCHEMA = 1
LABEL = re.compile(r"[a-z][a-z0-9-]{0,63}")
EFFORTS = ("off", "minimal", "low", "medium", "high", "xhigh", "max")
PROVIDERS = ("fake", "local")
MAX_REPEATS = 16
MAX_OUTPUT_TOKENS = 4096


class ManifestError(ValueError):
    """The campaign manifest or a pinned task fixture is invalid."""


def require(condition, message):
    if not condition:
        raise ManifestError(message)


def read_json(path):
    try:
        return json.loads(path.read_bytes())
    except ValueError as error:
        raise ManifestError(f"{path.name}: invalid JSON") from error


def load_task(base, entry):
    require(isinstance(entry, dict) and set(entry) == {"path", "sha256"},
            "each task entry needs exactly a path and a sha256")
    relative = entry["path"]
    parts = PurePosixPath(relative).parts if isinstance(relative, str) else ()
    require(parts and not relative.startswith("/") and ".." not in parts,
            "task paths must stay under the manifest directory")
    data = (base / relative).read_bytes()
    require(hashlib.sha256(data).hexdigest() == entry["sha256"], f"{relative}: digest mismatch")
    task = read_json(base / relative)
    require(isinstance(task, dict) and LABEL.fullmatch(str(task.get("id", ""))),
            f"{relative}: id must be a neutral label")
    require(isinstance(task.get("prompt"), str) and task["prompt"], f"{relative}: prompt required")
    require(isinstance(task.get("system", ""), str), f"{relative}: system must be text")
    tokens = task.get("max_output_tokens")
    require(isinstance(tokens, int) and 0 < tokens <= MAX_OUTPUT_TOKENS,
            f"{relative}: max_output_tokens out of range")
    tools = task.get("tools", [])
    require(isinstance(tools, list) and all(
        isinstance(tool, dict) and LABEL.fullmatch(str(tool.get("name", ""))) for tool in tools),
        f"{relative}: tools need neutral names")
    return {"path": relative, "sha256": entry["sha256"], "task": task}


def load_manifest(path):
    manifest = read_json(path)
    require(isinstance(manifest, dict), "manifest must be an object")
    require(manifest.get("schema") == SCHEMA, f"manifest schema must be {SCHEMA}")
    for field in ("campaign_id", "provider_id"):
        require(LABEL.fullmatch(str(manifest.get(field, ""))), f"{field} must be a neutral label")
    require(manifest.get("provider") in PROVIDERS, f"provider must be one of {PROVIDERS}")
    model = manifest.get("model")
    require(isinstance(model, str) and 0 < len(model.encode()) <= 256 and "\0" not in model,
            "model must be 1 to 256 bytes without NUL")
    require(manifest.get("effort") in EFFORTS, "effort is outside the contract vocabulary")
    seeds = manifest.get("seeds")
    require(isinstance(seeds, list) and seeds and len(set(seeds)) == len(seeds) and all(
        isinstance(seed, int) and not isinstance(seed, bool) and 0 <= seed < 2**32 for seed in seeds),
        "seeds must be unique unsigned 32-bit integers")
    repeats = manifest.get("repeats")
    require(isinstance(repeats, int) and not isinstance(repeats, bool) and 0 < repeats <= MAX_REPEATS,
            f"repeats must be 1 to {MAX_REPEATS}")
    require(isinstance(manifest.get("tasks"), list) and manifest["tasks"], "tasks required")
    tasks = [load_task(path.parent, entry) for entry in manifest["tasks"]]
    ids = [entry["task"]["id"] for entry in tasks]
    require(len(set(ids)) == len(ids), "task ids must be unique")
    return manifest, tasks


def request_body(manifest, task, seed):
    """The chat request; the effort is recorded but the chat wire format has no field for it."""
    messages = [{"role": "system", "content": task["system"]}] if task.get("system") else []
    messages.append({"role": "user", "content": task["prompt"]})
    body = {"model": manifest["model"], "stream": False, "seed": seed,
            "max_tokens": task["max_output_tokens"], "messages": messages}
    if task.get("tools"):
        body["tools"] = [{"type": "function", "function": tool} for tool in task["tools"]]
    return body


def outcome(manifest, complete, task, seed, repeat):
    body = request_body(manifest, task, seed)
    record = {"task": task["id"], "seed": seed, "repeat": repeat,
              "request_sha256": hashlib.sha256(endpoint.canonical(body).encode()).hexdigest()}
    try:
        reply = endpoint.parse_reply(complete(body))
    except endpoint.EndpointFailure as failure:
        record["status"] = "endpoint_failure"
        record["failure"] = {"category": failure.category, "status": failure.status}
        return record
    record["status"] = "tool_requested" if reply["tool_calls"] else "replied"
    record.update(reply)
    return record


def run_records(manifest, tasks, complete):
    return [outcome(manifest, complete, entry["task"], seed, repeat)
            for entry in tasks for seed in manifest["seeds"] for repeat in range(manifest["repeats"])]


def records_for(manifest, tasks, failure_status=None):
    """Records every call; `failure_status` makes the local stub refuse each one."""
    if manifest["provider"] == "fake":
        return run_records(manifest, tasks, endpoint.fake_reply)
    with endpoint.StubServer(failure_status) as stub:
        client = endpoint.LoopbackClient("127.0.0.1", stub.port)
        return run_records(manifest, tasks, client.complete)


def pretty(value):
    return json.dumps(value, sort_keys=True, indent=2, ensure_ascii=True) + "\n"


def run(manifest_path, out, failure_status=None):
    """Writes manifest.json and outcomes.jsonl under `out`; returns their bytes by name."""
    manifest, tasks = load_manifest(manifest_path)
    records = records_for(manifest, tasks, failure_status)
    outcomes = "".join(endpoint.canonical(record) + "\n" for record in records).encode()
    resolved = {key: manifest[key] for key in
                ("schema", "campaign_id", "provider", "provider_id", "model", "effort", "seeds", "repeats")}
    resolved["grading"] = "none"
    resolved["tasks"] = [{"id": entry["task"]["id"], "path": entry["path"], "sha256": entry["sha256"]}
                         for entry in tasks]
    resolved["outcomes"] = {"count": len(records), "sha256": hashlib.sha256(outcomes).hexdigest()}
    files = {"manifest.json": pretty(resolved).encode(), "outcomes.jsonl": outcomes}
    out.mkdir(parents=True, exist_ok=True)
    for name, data in files.items():
        (out / name).write_bytes(data)
    return files


def verify(manifest_path):
    """Runs the campaign twice and fails unless both runs wrote identical bytes."""
    with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
        runs = [run(manifest_path, Path(directory)) for directory in (first, second)]
    for name in runs[0]:
        digests = {hashlib.sha256(files[name]).hexdigest() for files in runs}
        if len(digests) != 1:
            raise ValueError(f"{name} differs between two runs")
        print(f"{name}: identical across two runs, sha256 {digests.pop()}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--out", type=Path, help="Directory for manifest.json and outcomes.jsonl")
    action.add_argument("--verify", action="store_true", help="Run twice and compare the bytes")
    args = parser.parse_args()
    try:
        if args.verify:
            verify(args.manifest)
        else:
            count = run(args.manifest, args.out)["outcomes.jsonl"].count(b"\n")
            print(f"Recorded {count} outcomes in {args.out}")
    except (OSError, ValueError) as error:
        print(f"Campaign failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
