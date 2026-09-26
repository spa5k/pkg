import pathlib,subprocess,hashlib,json,importlib.util,tempfile
out=pathlib.Path('/tmp/pkg-audit/mutations'); work=out/'workspace'; p=work/'tools/install/render.py'; original=p.read_bytes(); results=json.loads((out/'results.json').read_text())
# Verify the correct renderer contains independently computed fixture hashes.
spec=importlib.util.spec_from_file_location('control_render',p); control=importlib.util.module_from_spec(spec); spec.loader.exec_module(control)
with tempfile.TemporaryDirectory() as temp:
 assets=pathlib.Path(temp)
 for name in ['pkg-install-x86_64-linux','pkg-0.1.0-alpha.49-preview.pkg','install-preview.sh']:(assets/name).write_bytes(('unique bytes '+name).encode())
 rendered=control.render('v0.1.0-alpha.49',assets)
 results[0]['original_control_all_digests_present']=all(hashlib.sha256(a.read_bytes()).hexdigest() in rendered for a in assets.iterdir())
old='path.is_symlink() or '; assert original.decode().count(old)==1
try:
 p.write_text(original.decode().replace(old,'',1))
 cmd=['python3','-m','unittest','discover','-s','tools/install','-p','test_render.py','-v']; r=subprocess.run(cmd,cwd=work,capture_output=True,text=True); (out/'render-symlink-accepted.log').write_text(r.stdout+r.stderr)
 # Load without a stale pyc from the preceding source version.
 ns={'__file__':str(p),'__name__':'mutant_render_no_symlink'}; exec(compile(p.read_text(),str(p),'exec'),ns)
 with tempfile.TemporaryDirectory() as temp:
  assets=pathlib.Path(temp); target=assets/'payload'; target.write_bytes(b'linked payload')
  (assets/'pkg-install-x86_64-linux').symlink_to(target)
  (assets/'pkg-0.1.0-alpha.49-preview.pkg').write_bytes(b'mac payload'); (assets/'install-preview.sh').write_bytes(b'wrapper payload')
  accepted=bool(ns['render']('v0.1.0-alpha.49',assets))
  try:control.render('v0.1.0-alpha.49',assets); rejected=False
  except ValueError:rejected=True
 results.append({'id':'renderer-symlink-accepted','mutation':'Remove artifact symlink refusal','file':'tools/install/render.py','test_command':cmd,'mutant_test_exit':r.returncode,'complete_fixture_mutant_accepts_symlink':accepted,'complete_fixture_original_refuses_symlink':rejected,'restored':True})
finally:p.write_bytes(original)
assert p.read_bytes()==original
(out/'results.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps(results[-1],indent=2))
