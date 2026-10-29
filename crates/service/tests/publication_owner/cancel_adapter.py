"""Cancellation fixture: records actual Start, then waits without publishing."""
import json
import pathlib
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
pathlib.Path(sys.argv[1]).write_text(start["attempt_id"])
time.sleep(30)
