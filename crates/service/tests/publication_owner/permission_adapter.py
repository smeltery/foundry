"""Protocol fixture for a pending tool during fresh capture; no native state."""
import json
import sys
import time


def emit(value):
    print(json.dumps(value), flush=True)


emit({"type": "ready", "version": 1, "capabilities": ["context_capture_v1", "context_export_v1"]})
begin = json.loads(sys.stdin.readline())
assert begin["type"] == "context_capture_begin"
emit(dict(begin, type="context_capture_ready"))
start = json.loads(sys.stdin.readline())
assert start["type"] == "start"
emit({"type": "tool_call", "attempt_id": start["attempt_id"],
      "call_id": "read-once", "name": "read", "input": {"path": "note.txt"}})
time.sleep(30)
