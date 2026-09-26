import hashlib, importlib.util, json, os
from pathlib import Path
spec=importlib.util.spec_from_file_location('lifecycle','/Users/admin/pkg-proof/tests/release-lifecycle/check.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
m.require_disposable('TEST-DISPOSABLE-HOST')
assert not Path(m.CLI).exists() and not Path('/nix/receipt.json').exists()
work=Path('/Users/admin/alpha56-public');work.mkdir(mode=0o700)
c=m.Checks(work)
for key in ('GH_TOKEN','GITHUB_TOKEN'):
    c.env.pop(key,None)
script=work/'install.sh'
url='https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.56/install.sh'
c.run('Anonymous saved public installer',['curl','-fsSL','--proto','=https','--proto-redir','=https',url,'-o',str(script)])
assert hashlib.sha256(script.read_bytes()).hexdigest()=='f2411bdc25db7e90f5fd2d79022cd9f2767a6ca8af7944b3bcdf6a4b0e5e598c'
c.run('Verify public downloads',['/bin/sh',str(script),'--verify-only'])
c.run('Fresh public installation',['/bin/sh',str(script)])
c.installed_bytes(Path('/Users/admin/alpha56-proof/v0.1.0-alpha.56'))
c.run('Repeat public installer with configured PATH',['/bin/sh',str(script)])
c.run('Install cached package',[m.CLI,'install','fzf','-y'])
c.package('fzf','Run installed package')
for shell,rc in [('/bin/bash','.bash_profile'),('/bin/zsh','.zshrc')]:
    command=f'. "$HOME/{rc}"; . "$HOME/{rc}"; pkg --version; fzf --version; python3 -c \'import os; p=os.environ["PATH"].split(":"); assert sum(x.endswith("pkg/current/bin") for x in p)==1\''
    c.run('Fresh configured '+Path(shell).name,[shell,'-c',command])
c.run('Package verification',[m.CLI,'repair','--verify-only'])
c.run('Final public health',[m.CLI,'doctor'])
(work/'result.json').write_text(json.dumps({'status':'passed','sourceUrl':url,'installerSha256':hashlib.sha256(script.read_bytes()).hexdigest(),'anonymous':True,'checks':len(c.rows),'scope':'Fresh public reinstall after complete removal. No reboot in this phase.'},indent=2)+'\n')
print('PASS: anonymous public reinstall, repeat setup, package use, fresh shells, and health',flush=True)
