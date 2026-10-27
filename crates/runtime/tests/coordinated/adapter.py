import json,sys,pathlib,time,os
mode=sys.argv[1]
root=os.stat('.')
for descriptor in range(3,256):
    try: inherited=os.fstat(descriptor)
    except OSError: continue
    assert (inherited.st_dev,inherited.st_ino)!=(root.st_dev,root.st_ino), 'directory descriptor leaked'

def emit(value): print(json.dumps(value),flush=True)
emit({'type':'ready','version':1})
start=json.loads(sys.stdin.readline()); attempt=start['attempt_id']
if mode=='failed':
    emit({'type':'failed','attempt_id':attempt,'code':'private-code','message':'private-message'})
    sys.exit(0)
input={'path':'item'}
if mode=='inject': input['grants']=['read']
emit({'type':'tool_call','attempt_id':attempt,'call_id':'call','name':'read','input':input})
if mode=='premature':
    emit({'type':'settled','attempt_id':attempt,'outcome':'success','result':'too early'})
if mode=='parallel':
    emit({'type':'tool_call','attempt_id':attempt,'call_id':'second','name':'read','input':input})
if mode=='text':
    while not pathlib.Path('helper-started').exists(): time.sleep(.005)
    emit({'type':'text','attempt_id':attempt,'text':'progress'})
reply=json.loads(sys.stdin.readline())
if reply['type']=='cancel': sys.exit(0)
assert reply['type']=='tool_result' and reply['call_id']=='call'
pathlib.Path('adapter-result').write_text(json.dumps(reply))
if mode=='duplicate':
    emit({'type':'tool_call','attempt_id':attempt,'call_id':'call','name':'read','input':input})
else:
    emit({'type':'settled','attempt_id':attempt,'outcome':'success','result':'done'})
