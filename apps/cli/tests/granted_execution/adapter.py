import sys,json,os
assert 'FOUNDRY_TOOLHOST' not in os.environ
assert 'FOUNDRY_ADAPTER' not in os.environ
assert 'FOUNDRY_TEST_SECRET' not in os.environ
print(json.dumps({'type':'ready','version':1}),flush=True)
start=json.loads(sys.stdin.readline()); attempt=start['attempt_id']
name='write'
input={'path':'out.txt','content':'owned output','mode':'create'}
if mode=='overwrite': input['mode']='overwrite'
if mode=='read': name='read'; input={'path':'input.txt'}
if mode=='list': name='list'; input={'path':'.'}
print(json.dumps({'type':'tool_call','attempt_id':attempt,'call_id':'call','name':name,'input':input}),flush=True)
reply=json.loads(sys.stdin.readline())
assert reply['type']=='tool_result'
result=reply['result']
outcome='success' if result['status']=='ok' else 'action_required'
print(json.dumps({'type':'settled','attempt_id':attempt,'outcome':outcome,'result':json.dumps(result)}),flush=True)
