import json
import os
import struct
import sys
import time


def mark(name, value):
    fd = os.open(name, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, 0o600, dir_fd=3)
    os.write(fd, str(value).encode())
    os.close(fd)


mark("started", os.getpid())
if SCENARIO == "blocked_input":
    time.sleep(60)
size = struct.unpack(">I", sys.stdin.buffer.read(4))[0]
request = json.loads(sys.stdin.buffer.read(size))
assert sys.stdin.buffer.read() == b""
content = "ok"
if SCENARIO == "context":
    content = json.dumps({"pid": os.getpid(), "group": os.getpgrp(),
                          "cwd": os.getcwd(), "home": "HOME" in os.environ,
                          "path": "PATH" in os.environ})
if SCENARIO == "descriptor":
    fd, inode = map(int, request["input"]["path"].split(":"))
    try:
        content = "leaked" if os.fstat(fd).st_ino == inode else "closed"
    except OSError:
        content = "closed"
if SCENARIO == "read_capability":
    fd = os.open("value", os.O_RDONLY, dir_fd=3)
    content = os.read(fd, 100).decode()
    os.close(fd)
reply = {"version": 1, "attempt_id": request["attempt_id"],
         "call_id": request["call_id"], "result": {"status": "ok", "output": {"content": content}}}
if SCENARIO == "wrong_id":
    reply["call_id"] = "other"
if SCENARIO == "wrong_version":
    reply["version"] = 2
if SCENARIO == "wrong_shape":
    reply["result"]["output"] = {"bytes_written": 1}
if SCENARIO == "tool_error":
    reply["result"] = {"status": "error", "code": "denied", "effect": "none"}
if SCENARIO == "huge":
    sys.stdout.buffer.write(struct.pack(">I", 1048577))
    sys.stdout.buffer.flush()
    time.sleep(60)
if SCENARIO in ("descendant", "detached"):
    child = os.fork()
    if child == 0:
        if SCENARIO == "detached":
            os.setsid()
        mark("descendant", os.getpid())
        time.sleep(60)
        os._exit(0)
    # Ensure marker and session transition occur before the leader exits.
    for _ in range(1000):
        try:
            os.stat("descendant", dir_fd=3)
            break
        except FileNotFoundError:
            time.sleep(0.001)
if SCENARIO == "stderr":
    os.write(2, b"sensitive-fixture" * 100000)
body = json.dumps(reply, separators=(",", ":")).encode()
if SCENARIO == "duplicate":
    body = body.replace(b'"content":"ok"', b'"content":"secret","content":"ok"')
if SCENARIO == "unknown_error":
    body = body.replace(b'"status":"ok","output":{"content":"ok"}', b'"status":"error","code":"secret","effect":"none"')
frame = struct.pack(">I", len(body)) + body
sys.stdout.buffer.write(frame)
sys.stdout.buffer.flush()
if SCENARIO == "trailing":
    sys.stdout.buffer.write(frame)
    sys.stdout.buffer.flush()
if SCENARIO == "hang":
    mark("reply_sent", os.getpid())
    time.sleep(60)
if SCENARIO == "closed_hang":
    os.close(1)
    mark("reply_sent", os.getpid())
    time.sleep(60)
if SCENARIO == "nonzero":
    os._exit(7)
