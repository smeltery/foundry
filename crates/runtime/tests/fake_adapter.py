"""Authored, offline protocol fixture. Never executes requested tools."""

import json
import os
import subprocess
import sys
import time


def emit(payload):
    print(json.dumps(payload), flush=True)


scenario = sys.argv[1]
if scenario == "ready_timeout":
    time.sleep(10)
if scenario == "version":
    emit({"type": "ready", "version": 99})
    time.sleep(10)
if scenario == "frame":
    sys.stdout.write("x" * (2 * 1024 * 1024 + 1))
    sys.stdout.flush()
    time.sleep(10)
if scenario == "malformed":
    print("not-json secret-do-not-print", flush=True)
    time.sleep(10)
if scenario == "exit":
    sys.stderr.write("secret-do-not-print")
    sys.exit(2)
emit({"type": "ready", "version": 1})
if scenario == "blocked_input":
    time.sleep(10)
start = json.loads(sys.stdin.readline())
attempt = start["attempt_id"]
if scenario == "stale":
    emit({"type": "text", "attempt_id": "other", "text": "hidden"})
elif scenario == "duplicate_ready":
    emit({"type": "ready", "version": 1})
elif scenario == "cancel":
    emit({"type": "text", "attempt_id": attempt, "text": "started"})
    cancel = json.loads(sys.stdin.readline())
    assert cancel["type"] == "cancel" and cancel["mode"] == "graceful"
elif scenario == "hang":
    emit({"type": "text", "attempt_id": attempt, "text": "started"})
    time.sleep(10)
elif scenario == "child":
    subprocess.Popen([sys.executable, "-c", "import time,pathlib; time.sleep(.4); pathlib.Path('escaped').touch()"])
    emit({"type": "text", "attempt_id": attempt, "text": "started"})
    time.sleep(10)
elif scenario == "failed":
    emit({"type": "failed", "attempt_id": attempt, "code": "private", "message": "secret-do-not-print"})
    sys.exit(1)
elif scenario == "tool":
    emit({"type": "tool_call", "attempt_id": attempt, "call_id": "call", "name": "shell", "input": {}})
    reply = json.loads(sys.stdin.readline())
    assert reply["type"] == "tool_result"
    assert reply["call_id"] == "call"
    assert reply["result"]["status"] == "denied"
elif scenario == "stderr":
    sys.stderr.write("secret-do-not-print" * 200000)
    sys.stderr.flush()
else:
    emit({"type": "text", "attempt_id": attempt, "text": os.getcwd()})
    assert "HOME" not in os.environ and "OPENAI_API_KEY" not in os.environ
emit({"type": "settled", "attempt_id": attempt, "outcome": "success", "result": "done"})
