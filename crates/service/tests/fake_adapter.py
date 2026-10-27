"""Protocol-only adapter for service tests; performs no model or tool execution."""
import json
import pathlib
import sys
import time

print(json.dumps({"type": "ready", "version": 1}), flush=True)
start = json.loads(sys.stdin.readline())
attempt = start["attempt_id"]
mode = sys.argv[1]
if len(sys.argv) > 2:
    pathlib.Path(sys.argv[2]).write_text("started")
if mode == "wait":
    time.sleep(30)
elif mode == "quiet":
    time.sleep(0.8)
elif mode == "fail":
    print(json.dumps({"type": "failed", "attempt_id": attempt,
                      "code": "fixture-private-diagnostic", "message": "fixture-private-diagnostic"}), flush=True)
    sys.exit(1)
print(json.dumps({"type": "text", "attempt_id": attempt, "text": "test output"}), flush=True)
print(json.dumps({"type": "settled", "attempt_id": attempt,
                  "outcome": "success", "result": "test result"}), flush=True)
