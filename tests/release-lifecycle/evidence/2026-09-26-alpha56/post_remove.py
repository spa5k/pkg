import json,os,stat,subprocess
from pathlib import Path
assert os.geteuid()==0
assert subprocess.check_output(['/usr/sbin/sysctl','-n','hw.model'],text=True).startswith('VirtualMac')
root=Path('/Users/admin/alpha56-proof')
plan=json.loads(Path('/Users/admin/alpha56-ux/uninstall-json.stdout').read_text())
paths=[a['target'] for a in plan['plannedActions'] if a['action'] in ['Remove file','Remove directory'] and a['target'].startswith('/')]
paths.append('/Users/admin/Library/Application Support/pkg')
for path in paths: assert not os.path.lexists(path),path
services=[]
for service in ['org.pkg.nix-broker','org.pkg.root-helper']:
    p=subprocess.run(['/bin/launchctl','print','system/'+service],capture_output=True,text=True)
    assert p.returncode!=0, service
    services.append({'name':service,'exit':p.returncode,'stderr':p.stderr})
for name in ['pkg-nix-broker']:
    for kind in ['Users','Groups']:
        p=subprocess.run(['/usr/bin/dscl','.','-read','/'+kind+'/'+name],capture_output=True,text=True)
        assert p.returncode!=0,(kind,name)
coord=Path('/private/var/db/pkg-install-handoff.lock').stat()
assert coord.st_uid==0 and stat.S_IMODE(coord.st_mode)==0o600
assert not Path('/nix/receipt.json').exists() and not Path('/nix/nix-installer').exists()
record={'removedProductPaths':paths,'services':services,'brokerAccountAndGroupAbsent':True,'nixReceiptAndInstallerAbsent':True,'retainedCoordinationLock':{'owner':coord.st_uid,'mode':oct(stat.S_IMODE(coord.st_mode))},'verified':True}
(root/'removal-checks.json').write_text(json.dumps(record,indent=2)+'\n')
print('PASS: planned product paths, services, user state and broker account removed; vendor receipt/installer absent; coordination lock retained')
