import sys,json,hashlib,base64,time
mode=sys.argv[1];size=int(sys.argv[2])
def emit(value): print(json.dumps(value),flush=True)
emit({'type':'ready','version':1,'capabilities':['context_export_v1','context_restore_v1']})
command=json.loads(sys.stdin.readline())
if mode in ('capture', 'capture_legacy'):
    if mode=='capture':
        assert command['type']=='context_capture_begin'
        ready=dict(command,type='context_capture_ready')
    else:
        ready={'type':'context_capture_ready','attempt_id':'attempt','capture_id':'capture',
            'format_digest':'11'*32,'configuration_digest':'22'*32,
            'challenge':'01234567-89ab-4cde-8f01-23456789abcd'}
    emit(ready)
    if mode=='capture':
        assert json.loads(sys.stdin.readline())['type']=='start'
        emit({'type':'text','attempt_id':'attempt','text':'ordinary-after-handshake'})
    time.sleep(10)
    sys.exit(0)
payload=b'private-sentinel' if size==0 else bytes((i%256 for i in range(size)))
if mode=='receive':
    assert command['type']=='restore_payload'
    payload=base64.b64decode(command['payload_base64'],validate=True)
    assert len(payload)==size
frame={'type':'context_candidate','attempt_id':'attempt','capture_id':'capture',
    'challenge':'01234567-89ab-4cde-8f01-23456789abcd',
    'format':{'profile':'fixture','format_version':'1','producer_build':'owned'},
    'coverage':{'completed_turn_id':'turn','inputs':[],'results':[]},
    'payload_length':len(payload),'payload_digest':hashlib.sha256(payload).hexdigest(),
    'payload_base64':base64.b64encode(payload).decode()}
if mode=='wronghash': frame['payload_digest']='0'*64
if mode=='wrongattempt': frame['attempt_id']='foreign'
if mode=='unknownnested': frame['format']['unknown']='private-sentinel'
encoded=json.dumps(frame)
if mode=='duplicate': encoded=encoded.replace('"profile": "fixture"','"profile": "fixture", "profile": "fixture"')
if mode=='truncated':
    sys.stdout.write(encoded[:-1]);sys.stdout.flush();sys.exit(0)
if mode=='oversize': encoded='x'*(2*1024*1024+1)
print(encoded,flush=True)
if mode=='faultfull':
    emit({'type':'text','attempt_id':'attempt','text':'private-queued'})
    print('invalid-private-frame',flush=True)
    time.sleep(10)
    sys.exit(0)
if mode=='flood':
    for i in range(20): emit({'type':'context_failed','attempt_id':'attempt','operation_id':'op','code':'busy'})
emit({'type':'text','attempt_id':'attempt','text':'ordinary-after-context'})
time.sleep(10)
