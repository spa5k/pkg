import hashlib, json, os, platform, selectors, shlex, subprocess, time
from types import SimpleNamespace
from pathlib import Path
assert platform.system()=='Darwin'
assert subprocess.check_output(['/usr/sbin/sysctl','-n','hw.model'],text=True).startswith('VirtualMac')
ROOT=Path('/Users/admin/alpha56-ux'); ROOT.mkdir(exist_ok=True)
CLI='/usr/local/bin/pkg'
assert hashlib.sha256(Path(CLI).read_bytes()).hexdigest()=='bb915cd992bc665e98632ef789292bd36f93e0e53ca675881ee4f266e1f78299'
env=os.environ|{'CI':'true','NO_COLOR':'1','PATH':'/Users/admin/Library/Application Support/pkg/current/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin'}
rows=[]
def run(label,args):
    print('$ '+shlex.join(args),flush=True); start=time.monotonic()
    first_notice=None
    if label in ['preview-human','upgrade-preview']:
        proc=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,env=env)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(proc.stderr,selectors.EVENT_READ)
                assert selector.select(5), 'preview notice did not arrive within five seconds'
            first=proc.stderr.readline(); first_notice=round(time.monotonic()-start,3)
            assert first.startswith('Preparing the preview.'), first
            stdout,stderr=proc.communicate(timeout=1200)
            p=SimpleNamespace(stdout=stdout,stderr=first+stderr,returncode=proc.returncode)
        finally:
            if proc.poll() is None:
                proc.terminate(); proc.wait(timeout=10)
    else:
        p=subprocess.run(args,capture_output=True,text=True,env=env,timeout=1200)
    (ROOT/(label+'.stdout')).write_text(p.stdout); (ROOT/(label+'.stderr')).write_text(p.stderr)
    rows.append({'case':label,'command':args,'exit':p.returncode,'seconds':round(time.monotonic()-start,2),'firstNoticeSeconds':first_notice})
    (ROOT/'commands.json').write_text(json.dumps(rows,indent=2)+'\n')
    assert p.returncode==0,(label,p.stdout,p.stderr)
    print('PASS: '+label,flush=True); return p
human=run('uninstall-human',['sudo',CLI,'system','uninstall','--dry-run'])
plan=json.loads(run('uninstall-json',['sudo',CLI,'system','uninstall','--dry-run','--json']).stdout)
assert plan['actions']==len(plan['plannedActions'])==28
assert plan['retainedItems'] and plan['shellGuidance']
assert '/usr/local/bin/pkg' in human.stdout
p=run('preview-human',[CLI,'install','ripgrep','--dry-run'])
assert p.stderr.startswith('Preparing the preview.')
for mode in ['json','jsonl','quiet']:
    p=run('preview-'+mode,[CLI,'install','ripgrep','--dry-run','--'+mode])
    assert 'Preparing the preview.' not in p.stdout+p.stderr
    if mode=='json': assert json.loads(p.stdout)['ok']
    if mode=='jsonl':
        for line in p.stdout.splitlines(): json.loads(line)
p=run('upgrade-preview',[CLI,'upgrade','--all','--dry-run'])
assert p.stderr.startswith('Preparing the preview.')
p=run('retention-human',[CLI,'gc','--dry-run','--keep-generations','3'])
p=json.loads(run('retention-json',[CLI,'gc','--dry-run','--keep-generations','3','--max-age-days','0','--json']).stdout)
assert p['retention']=={'alwaysKeepActive':True,'keepRetiredGenerations':3,'maxAgeDays':0,'pruneRule':'outside-count-and-older-than-age'}
assert p['estimateScope']=='selected-generation-output-closures'
p=json.loads(run('gc-json',[CLI,'gc','--keep-generations','3','--max-age-days','0','--yes','--json']).stdout)
assert p['freedBytes'] is None
p=run('gc-human',[CLI,'gc','--keep-generations','3','--max-age-days','0','--yes'])
assert 'Space freed: unknown' in p.stdout
run('history',[CLI,'history'])
run('repair',[CLI,'repair','--verify-only'])
run('doctor',[CLI,'doctor'])
print('PASS: exact signed alpha.56 UX checks',flush=True)
