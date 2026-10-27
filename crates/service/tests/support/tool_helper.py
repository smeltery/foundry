"""Controlled framed helper fixture; not an implementation of workspace tools."""
import json
import os
import pathlib
import struct
import sys
import time

root = pathlib.Path(FIXTURE_ROOT)
mode = FIXTURE_MODE
size = struct.unpack('>I', sys.stdin.buffer.read(4))[0]
request = json.loads(sys.stdin.buffer.read(size))
assert sys.stdin.buffer.read() == b''
(root / 'helper_started').write_text(str(os.getpid()))
if mode == 'wait':
    time.sleep(1.4)
    (root / 'effect').write_text('must be stopped before this write')
    time.sleep(10)
if mode == 'barrier':
    while not (root / 'release').exists():
        time.sleep(.005)
if mode == 'effect_then_die':
    (root / 'effect').write_text('partial effects are not rolled back')
    sys.exit(2)
if mode == 'missing':
    sys.exit(2)
if mode == 'oversize':
    sys.stdout.buffer.write(struct.pack('>I', 1048577))
    sys.stdout.buffer.flush()
    sys.exit(0)
result = {'status': 'ok', 'output': {'content': 'offline content'}}
if mode == 'denied':
    result = {'status': 'error', 'code': 'denied', 'effect': 'none'}
if mode == 'uncertain':
    result = {'status': 'error', 'code': 'io', 'effect': 'unknown'}
reply = {'version': 1, 'attempt_id': request['attempt_id'], 'call_id': request['call_id'], 'result': result}
if mode == 'wrong':
    reply['call_id'] = 'foreign'
encoded = json.dumps(reply).encode()
sys.stdout.buffer.write(struct.pack('>I', len(encoded)) + encoded)
sys.stdout.buffer.flush()
if mode == 'bad_exit':
    sys.exit(2)
