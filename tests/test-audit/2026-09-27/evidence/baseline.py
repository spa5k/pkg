import ast, hashlib, json, pathlib, re, subprocess, time
root=pathlib.Path('/home/user/workspace'); out=pathlib.Path('/tmp/pkg-audit'); out.mkdir(exist_ok=True)
paths=[p for p in subprocess.check_output(['git','ls-files','-z'],cwd=root).decode().split('\0') if p and (root/p).is_file()]
manifest={p:hashlib.sha256((root/p).read_bytes()).hexdigest() for p in paths}
(out/'source-manifest.json').write_text(json.dumps({'base':'e96a7d06b01e0205e82f51f112a8951ae9324830','tracked_files':manifest,'scope':'tracked working tree after plan cleanup; excludes ignored and untracked user files'},indent=2)+'\n')
rows=[]
for name in paths:
 p=root/name
 if '/evidence/' in name: continue
 if p.suffix=='.py' and p.name.startswith('test_'):
  text=p.read_text(); tree=ast.parse(text)
  for node in ast.walk(tree):
   if isinstance(node,(ast.FunctionDef,ast.AsyncFunctionDef)) and node.name.startswith('test_'):
    rows.append({'file':name,'line':node.lineno,'end_line':node.end_lineno,'name':node.name,'language':'python','mark':'U','file_lines':len(text.splitlines())})
 elif p.suffix=='.rs':
  text=p.read_text()
  for m in re.finditer(r'#\[(?:tokio::)?test(?:\([^\]]*\))?\]\s*(?:#\[[^\]]*\]\s*)*(?:async\s+)?fn\s+(\w+)',text):
   rows.append({'file':name,'line':text.count('\n',0,m.start())+1,'name':m.group(1),'language':'rust','mark':'U','file_lines':len(text.splitlines())})
(out/'inventory.json').write_text(json.dumps({'method':'Python AST and Rust test-attribute discovery; macro-generated/property/doc tests require Cargo inventory','declarations':rows},indent=2)+'\n')
files=sorted({r['file'] for r in rows if r['language']=='python'})
results=[]
for index,name in enumerate(files):
 logname=name.replace('/','__')+'.log'; start=time.monotonic()
 command=['python3','-m','unittest','discover','-s',str(pathlib.Path(name).parent),'-p',pathlib.Path(name).name,'-v']
 try:
  r=subprocess.run(command,cwd=root,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=90)
  output=r.stdout; code=r.returncode
 except subprocess.TimeoutExpired as err:
  raw=err.stdout or b''; output=raw.decode(errors='replace') if isinstance(raw,bytes) else raw; code=124
 (out/logname).write_text(output)
 count=re.search(r'Ran (\d+) tests? in',output)
 results.append({'file':name,'command':command,'exit_code':code,'seconds':round(time.monotonic()-start,3),'tests_run':int(count[1]) if count else None,'log':logname})
 (out/'python-baseline.json').write_text(json.dumps(results,indent=2)+'\n')
 print(index+1,len(files),name,code,flush=True)
print('DONE',len(rows),'inventoried declarations',len(files),'Python files',flush=True)
