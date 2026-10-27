"""Synthetic protocol fixture: opaque bytes are not native provider state."""
import base64
import hashlib
import json
import os
import pathlib
import sys
import time

root = pathlib.Path(sys.argv[1])
result = json.loads(sys.argv[2])
(root / "pid").write_text(str(os.getpid()))

def emit(value):
    print(json.dumps(value), flush=True)

def read():
    return json.loads(sys.stdin.readline())

emit({"type": "ready", "version": 1, "capabilities": ["context_capture_v1", "context_export_v1"]})
begin = read()
assert begin["type"] == "context_capture_begin"
emit(dict(begin, type="context_capture_ready"))
start = read()
assert start["type"] == "start"
attempt = start["attempt_id"]
emit({"type": "settled", "attempt_id": attempt, "outcome": "action_required", "result": result})
export = read()
assert export["type"] == "context_export"
payload = b"\x00opaque\xff\n\x80fixture"
emit({"type": "context_candidate", "attempt_id": attempt,
      "capture_id": export["capture_id"], "challenge": export["challenge"],
      "format": {"profile": "wire-profile", "format_version": "wire-v3", "producer_build": "fixture-build"},
      "coverage": {"completed_turn_id": "turn", "inputs": [], "results": []},
      "payload_length": len(payload), "payload_digest": hashlib.sha256(payload).hexdigest(),
      "payload_base64": base64.b64encode(payload).decode()})
time.sleep(30)  # Real runtime cleanup must terminate/reap this owned process.
