import hashlib,json,subprocess,tarfile
from pathlib import Path
root=Path('/tmp/pkg-alpha56-20260926');assets=root/'assets'
identity='https://github.com/spa5k/pkg/.github/workflows/alpha-release.yml@refs/tags/v0.1.0-alpha.56'
files=sorted(p for p in assets.iterdir() if p.name!='1.root.json' and not p.name.endswith('.sigstore.json'))
assert len(files)==15, [p.name for p in files]
verified=[]
for path in files:
    with (root/(path.name+'.verification.log')).open('w') as log:
        subprocess.run(['cosign','verify-blob','--bundle',str(path)+'.sigstore.json','--certificate-identity',identity,'--certificate-oidc-issuer','https://token.actions.githubusercontent.com',str(path)],stdout=log,stderr=subprocess.STDOUT,check=True)
    verified.append({'name':path.name,'bytes':path.stat().st_size,'sha256':hashlib.sha256(path.read_bytes()).hexdigest()})
    print('verified',path.name,flush=True)
assert (assets/'1.root.json').read_bytes()==Path('/Users/pacific/.local/share/pkg-release/test/root.json').read_bytes()
for system in ['aarch64-darwin','x86_64-linux']:
    with tarfile.open(assets/f'pkg-binaries-{system}.tar.gz') as archive:
        members=archive.getmembers()
        expected={f'{name}-{system}' for name in ['pkg','pkg-install','pkg-nix-broker','pkg-root-helper']}
        assert {p.name for p in members}==expected
        for member in members:
            assert member.isfile()
            assert archive.extractfile(member).read()==(assets/member.name).read_bytes()
(root/'verified-assets.json').write_text(json.dumps({'source':'23a2de23eaaa61e1e10af4a52c1e83dbb24b13c4','tag':'v0.1.0-alpha.56','verified':verified},indent=2)+'\n')
