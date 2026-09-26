import json,os,pathlib,re,subprocess,time
root=pathlib.Path('/home/user/workspace');out=pathlib.Path('/tmp/pkg-cleanup');results=[]
env=os.environ|{'PATH':'/home/user/.cargo/bin:'+os.environ['PATH'],'CARGO_BUILD_JOBS':'1','CARGO_PROFILE_TEST_DEBUG':'0','CARGO_PROFILE_DEV_DEBUG':'0','RUSTUP_TOOLCHAIN':'1.96.1'}
def run(name,cmd,timeout=900):
 start=time.monotonic()
 with (out/(name+'.log')).open('w') as log:
  try: code=subprocess.run(cmd,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=timeout).returncode
  except subprocess.TimeoutExpired:code=124
 output=(out/(name+'.log')).read_text()
 results.append({'name':name,'command':cmd,'exit_code':code,'seconds':round(time.monotonic()-start,2),'summary':re.findall(r'(?:test result:.*|Ran \d+ tests? in.*|OK.*|FAILED.*)',output),'log':name+'.log'})
 (out/'validation.json').write_text(json.dumps(results,indent=2)+'\n');print(name,code,flush=True)
 return code
run('rust-after',['cargo','test','--workspace','--all-targets','--all-features','--locked'])
files=sorted(f for f in subprocess.check_output(['git','ls-files'],cwd=root,text=True).splitlines() if pathlib.Path(f).name.startswith('test_') and f.endswith('.py') and (root/f).is_file())
for f in files:
 run(f.replace('/','__'),['python3','-m','unittest','discover','-s',str(pathlib.Path(f).parent),'-p',pathlib.Path(f).name,'-v'],120)
run('bounded-capture',['python3','tests/linux-clean-host/test_untraced_vendor_replay.py'],120)
run('mutations',['python3','/tmp/pkg-cleanup/run_mutations.py'],1200)
(out/'verification.exit').write_text(str(int(any(r['exit_code'] for r in results)))+'\n')
print('Verification complete',flush=True)
