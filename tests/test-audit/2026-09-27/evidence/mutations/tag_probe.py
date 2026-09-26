import pathlib,subprocess,json,tempfile
out=pathlib.Path('/tmp/pkg-audit/mutations');work=out/'workspace';p=work/'tools/install/render.py';original=p.read_bytes();results=json.loads((out/'results.json').read_text())
original_ns={'__file__':str(p),'__name__':'original_render'};exec(compile(original,str(p),'exec'),original_ns)
assert original.decode().count('if re.fullmatch(')==1
try:
 p.write_text(original.decode().replace('if re.fullmatch(','if False and re.fullmatch(',1))
 cmd=['python3','-m','unittest','discover','-s','tools/install','-p','test_render.py','-v'];r=subprocess.run(cmd,cwd=work,capture_output=True,text=True);(out/'render-invalid-tag-accepted.log').write_text(r.stdout+r.stderr)
 mutant={'__file__':str(p),'__name__':'mutant_render'};exec(compile(p.read_text(),str(p),'exec'),mutant)
 with tempfile.TemporaryDirectory() as temp:
  assets=pathlib.Path(temp);tag="v1.0.0'; echo injected"
  for name in ['pkg-install-x86_64-linux',f'pkg-{tag[1:]}-preview.pkg','install-preview.sh']:(assets/name).write_bytes(b'fixture bytes')
  accepted=bool(mutant['render'](tag,assets))
  try:original_ns['render'](tag,assets);refused=False
  except ValueError:refused=True
 results.append({'id':'renderer-invalid-tag-accepted','mutation':'Disable release tag validation','file':'tools/install/render.py','test_command':cmd,'mutant_test_exit':r.returncode,'complete_fixture_mutant_accepts_invalid_tag':accepted,'complete_fixture_original_refuses_invalid_tag':refused,'rendered_script_executed':False,'restored':True})
finally:p.write_bytes(original)
assert p.read_bytes()==original
(out/'results.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps(results[-1],indent=2))
