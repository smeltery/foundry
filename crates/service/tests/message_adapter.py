import json
import pathlib
import sys
import time

mode = sys.argv[1]
marker = pathlib.Path(sys.argv[2])
ready = {"type": "ready", "version": 1}
if mode != "no_capability":
    ready["capabilities"] = ["message_ack_v2"]
print(json.dumps(ready), flush=True)
start = json.loads(sys.stdin.readline())
attempt = start["attempt_id"]
marker.write_text("ready")
if mode == "no_capability":
    time.sleep(0.1)
else:
    command = json.loads(sys.stdin.readline())
    assert command["type"] == "message"
    marker.with_suffix(".received").write_text(command["message_id"])
    if mode == "unacknowledged":
        time.sleep(30)
    if mode == "crash":
        sys.exit(1)
    ack = {"type": "message_accepted", "attempt_id": attempt,
           "message_id": command["message_id"], "receipt": command["receipt"]}
    if mode == "missing_receipt":
        del ack["receipt"]
    elif mode == "malformed_receipt":
        ack["receipt"] = 7
    elif mode == "wrong_receipt":
        ack["receipt"] = "not-the-delivery-challenge"
    print(json.dumps(ack), flush=True)
    print(json.dumps(ack), flush=True)
print(json.dumps({"type": "settled", "attempt_id": attempt,
                  "outcome": "success", "result": "fixture finished"}), flush=True)
