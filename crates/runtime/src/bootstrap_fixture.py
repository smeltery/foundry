import base64
import hashlib
import json
import os
from pathlib import Path
import sys
import time

mode = sys.argv[1]
Path('pid').write_text(str(os.getpid()))
if mode == 'blocked':
    time.sleep(30)
if mode == 'early':
    print(json.dumps({'type': 'ready', 'version': 1}), flush=True)
    time.sleep(30)
line = sys.stdin.buffer.readline(98305)
packet = json.loads(line)
body = base64.b64decode(packet['body_base64'], validate=True)
assert base64.b64encode(body).decode() == packet['body_base64']
assert len(body) <= 65536
assert set(os.environ) <= {'PATH', 'LANG', 'LC_ALL', 'TZ', '__CF_USER_TEXT_ENCODING'}
assert body.decode(errors='ignore') not in str(sys.argv)
Path('received').write_text(hashlib.sha256(body).hexdigest())
if mode == 'stderr':
    sys.stderr.buffer.write(body)
    sys.stderr.buffer.flush()
if mode == 'stall':
    time.sleep(30)
if mode == 'delayed':
    time.sleep(0.25)
ack = {'type': 'adapter_configured', 'version': packet['version'],
       'selection_digest': packet['selection_digest'], 'challenge': packet['challenge']}
if mode == 'wrong': ack['challenge'] = '00000000-0000-4000-8000-000000000000'
if mode == 'version': ack['version'] = 2
if mode == 'selection': ack['selection_digest'] = '0' * 64
if mode == 'extra': ack['secret'] = 'not-permitted'
if mode == 'oversize': ack['challenge'] = 'x' * 1100
if mode == 'duplicate_field':
    print(json.dumps(ack)[:-1] + ',"version":1}', flush=True)
else:
    print(json.dumps(ack), flush=True)
if mode == 'duplicate': print(json.dumps(ack), flush=True)
if mode == 'partial':
    sys.stdout.write('{'); sys.stdout.flush(); time.sleep(30)
if mode == 'no_ready' or mode == 'delayed': time.sleep(30)
print(json.dumps({'type': 'ready', 'version': 1}), flush=True)
for line in sys.stdin:
    command = json.loads(line)
    if command['type'] == 'start':
        Path('started').write_text('yes')
        print(json.dumps({'type': 'settled', 'attempt_id': command['attempt_id'],
                          'outcome': 'success', 'result': 'configured'}), flush=True)
    if command['type'] == 'cancel': break
