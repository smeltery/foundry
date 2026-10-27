import json
import os
import pathlib
import sys
import time

mode = sys.argv[1]
marker = pathlib.Path(sys.argv[2])
print(json.dumps({"type": "ready", "version": 1}), flush=True)
start = json.loads(sys.stdin.readline())
marker.write_text(str(os.getpid()))
if mode == "silent":
    time.sleep(30)
else:
    cancel = json.loads(sys.stdin.readline())
    assert cancel["type"] == "cancel" and cancel["mode"] == "graceful"
    marker.with_suffix(".graceful").write_text("received")
    if mode == "upgrade":
        time.sleep(30)
    else:
        print(json.dumps({"type": "settled", "attempt_id": start["attempt_id"],
                          "outcome": "success", "result": "must be suppressed"}), flush=True)
