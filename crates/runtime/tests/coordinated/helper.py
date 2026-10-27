import os,sys,json,struct,time
size=struct.unpack('>I',sys.stdin.buffer.read(4))[0]
request=json.loads(sys.stdin.buffer.read(size))
assert sys.stdin.buffer.read()==b''
fd=os.open('helper-started',os.O_WRONLY|os.O_CREAT,0o600,dir_fd=3)
os.write(fd,str(os.getpid()).encode());os.close(fd)
if mode=='hang': time.sleep(20)
if mode not in ('error','uncertain'):
    fd=os.open('effects',os.O_WRONLY|os.O_CREAT|os.O_APPEND,0o600,dir_fd=3)
    os.write(fd,b'x');os.close(fd)
result={'status':'ok','output':{'content':'ok'}}
if mode=='error': result={'status':'error','code':'invalid','effect':'none'}
if mode=='uncertain': result={'status':'error','code':'io','effect':'unknown'}
reply={'version':1,'attempt_id':request['attempt_id'],'call_id':request['call_id'],'result':result}
if mode=='bad': reply['attempt_id']='wrong'
encoded=json.dumps(reply).encode()
sys.stdout.buffer.write(struct.pack('>I',len(encoded))+encoded);sys.stdout.buffer.flush()
