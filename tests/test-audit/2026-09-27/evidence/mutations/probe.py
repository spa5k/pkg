import pathlib,subprocess,hashlib,json,importlib.util,tempfile
out=pathlib.Path('/tmp/pkg-audit/mutations'); work=out/'workspace'; results=[]
p=work/'tools/install/render.py'; original=p.read_bytes(); old='hashlib.file_digest(stream, "sha256").hexdigest()'; assert original.decode().count(old)==1
try:
 p.write_text(original.decode().replace(old,'("0" * 64)',1))
 cmd=['python3','-m','unittest','discover','-s','tools/install','-p','test_render.py','-v']
 r=subprocess.run(cmd,cwd=work,capture_output=True,text=True); (out/'render-zero-digests.log').write_text(r.stdout+r.stderr)
 spec=importlib.util.spec_from_file_location('mutant_render',p); module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
 with tempfile.TemporaryDirectory() as temporary:
  assets=pathlib.Path(temporary)
  for name in ['pkg-install-x86_64-linux','pkg-0.1.0-alpha.49-preview.pkg','install-preview.sh']:(assets/name).write_bytes(('unique bytes '+name).encode())
  rendered=module.render('v0.1.0-alpha.49',assets)
  control={a.name:{'expected_sha256':hashlib.sha256(a.read_bytes()).hexdigest(),'correct_digest_in_render':hashlib.sha256(a.read_bytes()).hexdigest() in rendered} for a in assets.iterdir()}
 results.append({'id':'renderer-wrong-sha256','mutation':'Replace actual artifact SHA256 with 64 zeroes','file':'tools/install/render.py','test_command':cmd,'mutant_test_exit':r.returncode,'independent_control':control,'restored':True})
finally:p.write_bytes(original)
assert p.read_bytes()==original
p=work/'tests/linux-clean-host/run.sh'; original=p.read_bytes()
try:
 first,rest=original.split(b'\n',1); p.write_bytes(first+b'\nexit 0 # audit mutation: skip the proof\n'+rest)
 cmd=['python3','-m','unittest','discover','-s','tools/release','-p','test_workflow.py','-v']; r=subprocess.run(cmd,cwd=work,capture_output=True,text=True);(out/'skipped-linux-proof.log').write_text(r.stdout+r.stderr)
 control=subprocess.run(['/bin/sh',str(p),'--invalid-audit-input'],cwd=work,capture_output=True,text=True)
finally:p.write_bytes(original)
assert p.read_bytes()==original
original_control=subprocess.run(['/bin/sh',str(p),'--invalid-audit-input'],cwd=work,capture_output=True,text=True)
results.append({'id':'skipped-linux-proof','mutation':'Insert a successful early exit before all proof checks','file':'tests/linux-clean-host/run.sh','test_command':cmd,'mutant_test_exit':r.returncode,'mutant_invalid_input_exit':control.returncode,'original_invalid_input_exit':original_control.returncode,'restored':True})
(out/'results.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps(results,indent=2))
