"""Transport-only loading peer; never a native validation fixture."""
import json
import sys


def emit(value):
    print(json.dumps(value), flush=True)


emit({"type": "ready", "version": 1, "capabilities": ["context_restore_v2"]})
begin = json.loads(sys.stdin.readline())
assert begin["type"] == "restore_begin_v2"
assert "attempt_id" not in begin
emit({"type": "restore_prepared_v2", "scope": begin["scope"],
      "begin_binding": sys.argv[1]})
payload = json.loads(sys.stdin.readline())
assert payload["type"] == "restore_payload_v2"
assert "attempt_id" not in payload
emit({"type": "context_restored_v2", "scope": payload["scope"],
      "begin_binding": payload["begin_binding"],
      "payload_digest": payload["payload_digest"], "receipt": payload["receipt"]})
assert not sys.stdin.readline(), "no execution command permitted"
