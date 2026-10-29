import hashlib, json, os, sys, time
raw = sys.stdin.buffer.read()
request = json.loads(raw)
mode = request['input']['mode']
if mode == 'descendant':
    if os.fork() == 0:
        time.sleep(30)
        os._exit(0)
    sys.exit(0)
if mode == 'stderr':
    sys.stderr.write('private fixture diagnostic')
reply = dict(version=1, request_id=request['request_id'], operation=request['operation'],
             registration_digest=request['registration_digest'], request_digest=hashlib.sha256(raw).hexdigest(),
             result=dict(status='ok', value=dict(accepted=True)))
if mode == 'digest':
    reply['request_digest'] = '00' * 32
if mode == 'identity':
    reply['request_id'] = 'other'
if mode == 'operation':
    reply['operation'] = 'observe'
if mode == 'unknown':
    reply['other'] = True
encoded = json.dumps(reply, separators=(',', ':'))
if mode == 'duplicate':
    encoded = encoded[:-1] + ',"version":1}'
if mode == 'trailing':
    encoded += '{}'
if mode == 'oversize':
    encoded = 'x' * (2 * 1048576 + 1)
sys.stdout.write(encoded)
