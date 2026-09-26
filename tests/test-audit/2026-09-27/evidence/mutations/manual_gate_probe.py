import pathlib,subprocess,json
out=pathlib.Path('/tmp/pkg-audit/mutations');work=out/'workspace';p=work/'.github/workflows/release.yml';original=p.read_bytes();results=json.loads((out/'results.json').read_text())
old="if: ${{ github.event_name == 'workflow_dispatch' && inputs.production-linux }}"; new='if: ${{ true }}';assert original.decode().count(old)==1
try:
 p.write_text(original.decode().replace(old,new,1))
 cmd=['python3','-m','unittest','discover','-s','tools/release','-p','test_workflow.py','-v'];r=subprocess.run(cmd,cwd=work,capture_output=True,text=True);(out/'production-manual-gate-removed.log').write_text(r.stdout+r.stderr)
 results.append({'id':'production-manual-gate-removed','mutation':'Change the production-linux job condition to unconditional true','file':'.github/workflows/release.yml','original_job_condition':old,'mutant_job_condition':new,'test_command':cmd,'mutant_test_exit':r.returncode,'workflow_dispatched':False,'restored':True})
finally:p.write_bytes(original)
assert p.read_bytes()==original
(out/'results.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps(results[-1],indent=2))
