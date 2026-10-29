import base64,hashlib,json,os,pathlib,sys,time
root=pathlib.Path(sys.argv[1]);mode=sys.argv[2]
(root/'pid').write_text(str(os.getpid()))
def emit(value): print(json.dumps(value),flush=True)
def read():
    raw=sys.stdin.readline()
    value=json.loads(raw)
    if value['type']=='tool_result':
        (root/'result-body').write_text(raw.split('"result":',1)[1][:-2])
    with (root/'commands').open('a') as output: output.write(json.dumps(value)+'\n')
    return value
if mode=='delayed_ready': time.sleep(0.4)
caps=['context_capture_v1','context_export_v1','message_ack_v2']
if mode=='missing': caps.remove('context_capture_v1')
if mode=='missing_export': caps.remove('context_export_v1')
if mode=='no_messages': caps.remove('message_ack_v2')
emit({'type':'ready','version':1,'capabilities':caps})
if mode=='premature': emit({'type':'text','attempt_id':'attempt','text':'premature'})
begin=read()
assert begin['type']=='context_capture_begin'
if mode=='timeout': time.sleep(10)
ready=dict(begin,type='context_capture_ready')
if mode in ('capture_id','attempt_id'): ready[mode]='wrong'
if mode in ('format_digest','configuration_digest'): ready[mode]='00'*32
if mode=='challenge': ready['challenge']='01234567-89ab-4cde-8f01-23456789abcd'
if mode=='restored':
    emit({'type':'restore_prepared','attempt_id':'attempt','restore_id':'old',
          'format_digest':begin['format_digest'],'configuration_digest':begin['configuration_digest']})
else: emit(ready)
start=read()
assert start['type']=='start' and start['attempt_id']=='attempt'
assert set(start)=={'type','version','attempt_id','session'}
if mode in ('tool','tool_terminal'):
    emit({'type':'tool_call','attempt_id':'attempt','call_id':'call','name':'read','input':{'path':'file'}})
else:
    emit({'type':'text','attempt_id':'attempt','text':'started'})
if mode.startswith('terminal'):
    emit({'type':'settled','attempt_id':'attempt','outcome':'success','result':'private-final'})
while True:
    command=read()
    if command['type']=='cancel': sys.exit(0)
    if command['type']=='tool_result':
        emit({'type':'text','attempt_id':'attempt','text':'tool-result'})
        if mode=='tool_terminal': emit({'type':'settled','attempt_id':'attempt','outcome':'success','result':'private-final'})
    if command['type']=='message':
        ack={key:command[key] for key in ('attempt_id','message_id','receipt')}
        emit(dict(ack,type='message_accepted'))
        if mode=='settle': emit({'type':'settled','attempt_id':'attempt','outcome':'success','result':'unsealed'})
    if command['type']=='context_export':
        payload=b'opaque-offline-fixture'
        emit({'type':'context_candidate','attempt_id':'attempt','capture_id':command['capture_id'],
              'challenge':command['challenge'],'format':{'profile':'wire-profile','format_version':'1','producer_build':'fixture'},
              'coverage':{'completed_turn_id':'turn','inputs':[],'results':[]},
              'payload_length':len(payload),'payload_digest':hashlib.sha256(payload).hexdigest(),
              'payload_base64':base64.b64encode(payload).decode()})
        if mode=='terminal_exit': sys.exit(0)
        if mode=='terminal_bad': print('not-json',flush=True)
        if mode=='terminal_late': emit({'type':'text','attempt_id':'attempt','text':'forbidden'})
