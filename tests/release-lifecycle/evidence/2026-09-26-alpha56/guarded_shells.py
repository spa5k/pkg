import json, os, shlex, subprocess, sys
from pathlib import Path
assert subprocess.check_output(['/usr/sbin/sysctl','-n','hw.model'],text=True).startswith('VirtualMac')
root=Path('/Users/admin/alpha56-proof'); mode=sys.argv[1];assert mode in ['before','after']
wrapper=(root/'v0.1.0-alpha.56/install-preview.sh').read_text()
guard=next(line.strip() for line in wrapper.split("'") if line.startswith('  [ ! -x /usr/local/bin/pkg ]'))
rows=[]
for shell,rc in [('/bin/bash','.bash_profile'),('/bin/zsh','.zshrc')]:
    path=Path.home()/rc
    if mode=='before':
        (root/(rc[1:]+'.before-fixture')).write_bytes(path.read_bytes() if path.exists() else b'')
        with path.open('a') as f: f.write('\n# alpha56 disposable VM startup proof\n'+guard+'\n')
    assert guard in path.read_text(), 'uninstall removed personal startup guidance'
    command='. '+shlex.quote(str(path))+'; . '+shlex.quote(str(path))
    if mode=='before':
        command += """; /usr/local/bin/pkg --version; printf '%s\\n' "$PATH" | /usr/bin/awk -F: '{n=0;for(i=1;i<=NF;i++)if($i ~ /pkg\\/current\\/bin$/)n++;if(n != 1)exit 1;print "managed PATH entries:",n}'"""
    else: command+='; test ! -x /usr/local/bin/pkg'
    p=subprocess.run([shell,'-c',command],text=True,capture_output=True,env=os.environ|{'PATH':'/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'})
    (root/f'shell-{mode}-{Path(shell).name}.stdout').write_text(p.stdout)
    (root/f'shell-{mode}-{Path(shell).name}.stderr').write_text(p.stderr)
    rows.append({'shell':shell,'startup':str(path),'command':command,'exit':p.returncode})
    assert p.returncode==0 and 'no such file' not in p.stderr.lower() and 'not found' not in p.stderr.lower(),(shell,p.stdout,p.stderr)
(root/f'shell-{mode}.json').write_text(json.dumps(rows,indent=2)+'\n')
print('PASS: actual signed wrapper guidance in fresh Bash/zsh '+mode+' removal')
