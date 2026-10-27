"""Offline adapter checking that receipt commits precede adapter tool results."""
import json
import os
import pathlib
import sqlite3
import sys
import time

database, directory, mode = sys.argv[1:]
root = pathlib.Path(directory)
print(json.dumps({'type': 'ready', 'version': 1}), flush=True)
start = json.loads(sys.stdin.readline())
attempt = start['attempt_id']
(root / 'adapter_started').write_text(str(os.getpid()))
if mode == 'call_barrier':
    while not (root / 'call_release').exists():
        time.sleep(.005)
args = {'path': 'note.txt'}
name = 'read'
if mode == 'injected':
    args['grants'] = ['write.overwrite']
if mode in ('write', 'effect'):
    name = 'write'
    args = {'path': 'output.txt', 'content': 'offline content', 'mode': 'create'}
call = {'type': 'tool_call', 'attempt_id': attempt, 'call_id': 'call', 'name': name, 'input': args}
print(json.dumps(call), flush=True)
if mode == 'premature':
    print(json.dumps({'type': 'settled', 'attempt_id': attempt, 'outcome': 'success', 'result': 'premature'}), flush=True)
if mode == 'text_after_helper':
    while not (root / 'helper_started').exists():
        time.sleep(.005)
    while not (root / 'text_release').exists():
        time.sleep(.005)
    print(json.dumps({'type': 'text', 'attempt_id': attempt, 'text': 'offline event'}), flush=True)
    (root / 'text_sent').write_text('sent')
message = json.loads(sys.stdin.readline())
assert message['type'] == 'tool_result'
with sqlite3.connect(database) as db:
    row = db.execute('SELECT outcome FROM tool_calls WHERE call_id=?', ('call',)).fetchone()
(root / 'observed').write_text(json.dumps({'result': message['result'], 'outcome': row[0] if row else None}))
if message['result'].get('status') != 'denied':
    assert row and row[0] in ('success', 'failed')
if mode == 'duplicate':
    print(json.dumps(call), flush=True)
    time.sleep(10)
print(json.dumps({'type': 'settled', 'attempt_id': attempt, 'outcome': 'success', 'result': 'fixture complete'}), flush=True)
time.sleep(10)
