import json
import os
import sys
import time

mode = sys.argv[1]
ready = {"type": "ready", "version": 1}
if mode != "no_capability":
    ready["capabilities"] = ["message_ack_v2"]
if mode == "legacy_capability":
    ready["capabilities"] = ["message_ack_v1"]
print(json.dumps(ready), flush=True)
def read_line():
    data = bytearray()
    while not data.endswith(b"\n"):
        chunk = os.read(0, 1)
        if not chunk:
            raise EOFError()
        data.extend(chunk)
    return json.loads(data)

start = read_line()
attempt = start["attempt_id"]
if mode == "unsent":
    print(json.dumps({"type": "message_accepted", "attempt_id": attempt,
                      "message_id": "m", "receipt": "unknown"}), flush=True)
    time.sleep(1)
if mode in ("no_capability", "legacy_capability"):
    print(json.dumps({"type": "settled", "attempt_id": attempt,
                      "outcome": "success", "result": "no messages"}), flush=True)
    sys.exit(0)
if mode == "preack_large":
    # Observe a partial message while leaving its tail (and receipt) unread.
    prefix = os.read(0, 4096)
    assert prefix and b'"text":' in prefix and b'"receipt":' not in prefix
    print(json.dumps({"type": "message_accepted", "attempt_id": attempt,
                      "message_id": "m", "receipt": "unknown"}), flush=True)
    time.sleep(.2)
    # The old implementation unblocked its validator only after this read.
    message = json.loads(prefix + sys.stdin.buffer.readline())
    assert len(message["text"]) == 262144
else:
    message = json.loads(sys.stdin.buffer.readline())
assert message["type"] == "message"
ack = {"type": "message_accepted", "attempt_id": attempt,
       "message_id": message["message_id"], "receipt": message["receipt"]}
if mode == "foreign":
    ack["message_id"] = "other"
if mode == "stale_attempt":
    ack["attempt_id"] = "other"
print(json.dumps(ack), flush=True)
if mode == "duplicate":
    print(json.dumps(ack), flush=True)
print(json.dumps({"type": "settled", "attempt_id": attempt,
                  "outcome": "success", "result": message["text"]}), flush=True)
