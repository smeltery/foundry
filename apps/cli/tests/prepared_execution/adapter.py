"""Synthetic private-bootstrap peer; never an SDK factory."""
import base64
import hashlib
import json
import os
from pathlib import Path
import sys
import time


def emit(value):
    print(json.dumps(value), flush=True)


assert set(os.environ) <= {'PATH', 'LANG', 'LC_ALL', 'TZ', '__CF_USER_TEXT_ENCODING'}
assert len(sys.argv) == 1
packet = json.loads(sys.stdin.readline())
assert packet['type'] == 'adapter_bootstrap'
body = json.loads(base64.b64decode(packet['body_base64'], validate=True))
assert body['version'] == 1
assert body['private']['credential']['value'] == 'private-key-canary'
assert body['private']['system_prompt'] == 'private-system-canary'
selection = body['selection']
encoded = bytearray(b'foundry.adapter.selection.v1\0')


def text(value):
    raw = value.encode()
    encoded.extend(len(raw).to_bytes(8, 'big'))
    encoded.extend(raw)


for field in ['provider', 'model', 'effort', 'directory']:
    text(selection[field])
encoded.extend(bytes.fromhex(selection['workspace_identity']))
text(selection['profile'])
encoded.extend(selection['profile_version'].to_bytes(8, 'big'))
text(selection['permission_profile'])
text(selection['resource_policy'])
encoded.extend(bytes.fromhex(selection['resource_identity']))
text(selection['tool_catalog'])
encoded.extend(len(selection['tools']).to_bytes(8, 'big'))
for tool in selection['tools']:
    text(tool)
encoded.extend(bytes.fromhex(selection['policy_identity']))
for field in ['generation', 'max_output_bytes', 'max_pending_text_bytes']:
    encoded.extend(selection[field].to_bytes(8, 'big'))
assert hashlib.sha256(encoded).hexdigest() == packet['selection_digest']
Path('received.json').write_text(json.dumps({'selection': selection, 'digest': packet['selection_digest']}))
if mode == 'reject':
    sys.stderr.write('private-key-canary private-system-canary')
    sys.exit(1)
ack = {'type': 'adapter_configured', 'version': packet['version'],
       'selection_digest': packet['selection_digest'], 'challenge': packet['challenge']}
if mode == 'wrong_ack':
    ack['selection_digest'] = '00' * 32
emit(ack)
emit({'type': 'ready', 'version': 1})
start = json.loads(sys.stdin.readline())
assert start['type'] == 'start'
assert start['session']['directory'] == selection['directory']
assert start['session']['model'] == {
    'provider': selection['provider'], 'model': selection['model'], 'effort': selection['effort']}
Path('started').write_text(start['attempt_id'])
if mode in ['pending', 'write']:
    emit({'type': 'tool_call', 'attempt_id': start['attempt_id'], 'call_id': 'call',
          'name': 'write', 'input': {'path': 'out.txt', 'content': 'owned output', 'mode': 'create'}})
    reply = json.loads(sys.stdin.readline())
    assert reply['type'] == 'tool_result'
    assert reply['result']['status'] == 'ok'
emit({'type': 'settled', 'attempt_id': start['attempt_id'], 'outcome': 'success', 'result': 'configured'})
time.sleep(30)
