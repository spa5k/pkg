from concurrent.futures import ThreadPoolExecutor
from pathlib import Path,PurePosixPath
import brotli,hashlib,json,re,subprocess,tarfile
BASE=Path('/tmp/pkg-alpha56-20260926')
OUT=BASE/'review-spec-artifacts'
SOURCE='23a2de23eaaa61e1e10af4a52c1e83dbb24b13c4'
TAG='v0.1.0-alpha.56'
ARCHIVE='fb1144d97ba7d9f58ea7c869028ea47e30d3aa92769be15eb3f3945772daa4ba'
ROOT='2f54413cd26fd913067c6aceba21191b45bad0122296669c647009765b6787cc'
REPO=Path('/Users/pacific/Developer/pkg')
def sha(x): return hashlib.sha256(x).hexdigest()
def data(p):
 assert p.is_file() and not p.is_symlink(),p
 return p.read_bytes()
def read(name): return json.loads(data(BASE/name))
def check(blob,length,digest):
 assert len(blob)==length,(len(blob),length)
 assert sha(blob)==digest,(sha(blob),digest)
assert subprocess.check_output(['git','rev-parse',TAG+'^{}'],cwd=REPO,text=True).strip()==SOURCE
inventory=read('channel-files.json'); expected={x['path']:x for x in inventory}
assert len(expected)==len(inventory)==25
assert {str(p.relative_to(BASE/'channel')) for p in (BASE/'channel').rglob('*') if p.is_file()}==set(expected)
archive=BASE/'release-assets/channel.tar.gz'
assert sha(data(archive))==ARCHIVE
with tarfile.open(archive) as t:
 members=t.getmembers()
 assert len(members)==25 and {m.name for m in members}==set(expected)
 for m in members:
  assert m.isfile() and not PurePosixPath(m.name).is_absolute() and '..' not in PurePosixPath(m.name).parts,m.name
  blob=t.extractfile(m).read(); record=expected[m.name]
  check(blob,record['length'],record['sha256'])
  assert blob==data(BASE/'channel'/m.name)
print('Closed channel archive: 25 unique regular files; no links, traversal, or extra files; all hashes and bytes match.',flush=True)
verified=read('verified-assets.json')
assert verified['source']==SOURCE and verified['tag']==TAG and len(verified['verified'])==15
assets={x['name']:x for x in verified['verified']}
assert len(assets)==15
assert len(list((BASE/'assets').iterdir()))==31
for name,r in assets.items(): check(data(BASE/'assets'/name),r['bytes'],r['sha256'])
identity='https://github.com/spa5k/pkg/.github/workflows/alpha-release.yml@refs/tags/'+TAG
issuer='https://token.actions.githubusercontent.com'
def cosign(name):
 p=BASE/'assets'/name
 result=subprocess.run(['/opt/homebrew/bin/cosign','verify-blob','--bundle',str(p)+'.sigstore.json','--certificate-identity',identity,'--certificate-oidc-issuer',issuer,str(p)],capture_output=True,text=True)
 (OUT/(name+'.cosign.log')).write_text(result.stdout+result.stderr)
 assert result.returncode==0,(name,result.stdout,result.stderr)
 return name
with ThreadPoolExecutor(max_workers=4) as pool: names=list(pool.map(cosign,assets))
print('Cosign: 15 of 15 verified against exact alpha.56 workflow identity and GitHub OIDC issuer.',flush=True)
root=read('channel/metadata/1.root.json')
assert sha(data(BASE/'channel/metadata/1.root.json'))==ROOT
assert data(BASE/'channel/root.json')==data(BASE/'channel/metadata/1.root.json')
assert data(BASE/'assets/1.root.json')==data(BASE/'channel/metadata/1.root.json')
assert root['signed']['pkgEnvironment']=='test'
assert all(x['threshold']==1 for x in root['signed']['roles'].values())
channel_result=subprocess.run(['python3',str(BASE/'verify_channel.py'),str(BASE/'channel'),'56'],capture_output=True,text=True,check=True)
(OUT/'channel-signatures.log').write_text(channel_result.stdout+channel_result.stderr)
assert json.loads(channel_result.stdout)['verified']
targets=read('channel/metadata/56.targets.json')['signed']['targets']; assert len(targets)==18
manifest=read('channel/release-manifest.json')
assert manifest['releaseId']=='alpha-56' and manifest['channelSequence']==manifest['timestampVersion']==56 and manifest['trustedRootSha256']==ROOT
records=manifest['artifacts']+manifest['determinate']['artifacts']
assert len(records)==18 and {r['target'] for r in records}==set(targets)
def target(name):
 r=targets[name]
 p=BASE/'channel/targets'/(r['hashes']['sha256']+'.'+name)
 blob=data(p);check(blob,r['length'],r['hashes']['sha256']);return blob
for r in records:
 record=targets[r['target']]
 assert r['length']==record['length'] and r['sha256']==record['hashes']['sha256']
 assert target(r['target'])==data(BASE/'staged-targets'/r['source'])
 if r['kind']=='installer-payload': assert target(r['target'])==data(BASE/'assets'/(Path(r['target']).name+'-'+r['system']))
card=read('release-assets/release-card.json')
assert card['environment']=='test' and card['sequence']==56 and card['root_sha256']==ROOT and card['product_commit']==SOURCE
assert len(card['targets'])==18 and {r['name'] for r in card['targets']}==set(targets)
for r in card['targets']: check(target(r['name']),r['length'],r['sha256'])
for system in ['aarch64-darwin','x86_64-linux']:
 names={f'{name}-{system}' for name in ['pkg','pkg-install','pkg-nix-broker','pkg-root-helper']}
 with tarfile.open(BASE/'assets'/f'pkg-binaries-{system}.tar.gz') as t:
  members=t.getmembers(); assert len(members)==4 and {m.name for m in members}==names
  for m in members: assert m.isfile() and t.extractfile(m).read()==data(BASE/'assets'/m.name)
print('All 18 TUF targets match signed lengths, digests, release card, and staged bytes. All six installer payloads and both binary archives match Cosign-verified individual assets.',flush=True)
descriptor=json.loads(target('descriptor.json'))
assert descriptor['channel']=='pkg-alpha' and descriptor['sequence']==56
assert descriptor['nixpkgs']=={'owner':'NixOS','repo':'nixpkgs','rev':'a62e6edd6d5e1fa0329b8653c801147986f8d446','narHash':'sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw='}
assert descriptor['index']['source']=='self-built'
counts={}
for system,binding in descriptor['index']['perSystem'].items():
 assert binding['sha256']==targets[binding['target']]['hashes']['sha256']
 index=json.loads(brotli.decompress(target(binding['target'])))
 assert index['channelSeq']==56 and index['system']==system and index['nixpkgsRev']==descriptor['nixpkgs']['rev'] and index['source']=='self-built'
 counts[system]=len(index['records'])
assert counts=={'aarch64-darwin':64412,'x86_64-linux':69517}
for system,binding in descriptor['nixRuntime']['perSystem'].items():
 assert binding['assetManifestSha256']==targets[binding['assetManifestTarget']]['hashes']['sha256']
 assert binding['sha256']==targets[f'nix/2.34.8/{system}.tar.xz']['hashes']['sha256']
 assert binding['url']==f'https://releases.nixos.org/nix/nix-2.34.8/nix-2.34.8-{system}.tar.xz'
print('Descriptor binds the unchanged Nixpkgs pin, Nix runtime, and sequence 56 indexes: 64,412 macOS records and 69,517 Linux records.',flush=True)
installer=data(BASE/'assets/install.sh').decode()
values=dict(re.findall(r"^(PKG_[A-Z0-9_]+)='([^']*)'$",installer,re.M))
assert values['PKG_RELEASE']==TAG
mapping={'PKG_SHA256_X86_64_LINUX':'pkg-install-x86_64-linux','PKG_SHA256_MACOS_PACKAGE':'pkg-0.1.0-alpha.56-preview.pkg','PKG_SHA256_MACOS_WRAPPER':'install-preview.sh'}
for key,name in mapping.items(): assert values[key]==assets[name]['sha256']
template=subprocess.check_output(['git','show',TAG+':docs/install.sh'],cwd=REPO,text=True)
for key,value in values.items(): template=template.replace('@'+key+'@',value)
assert template.encode()==data(BASE/'assets/install.sh')
assert sha(template.encode())=='f2411bdc25db7e90f5fd2d79022cd9f2767a6ca8af7944b3bcdf6a4b0e5e598c'
wrapper=data(BASE/'assets/install-preview.sh').decode()
assert wrapper==subprocess.check_output(['git','show',TAG+':packaging/macos/install-preview.sh'],cwd=REPO,text=True)
assert wrapper.count('[ ! -x /usr/local/bin/pkg ] || eval "$(/usr/local/bin/pkg shellenv)"')==2
print('Bootstrap exactly matches the tagged template with verified asset pins. Both signed wrapper setup lines have the executable guard.',flush=True)
summary={'source':SOURCE,'tag':TAG,'archiveSha256':ARCHIVE,'rootSha256':ROOT,'files':25,'cosignAssets':15,'tufTargets':18,'indexes':counts,'status':'verified','limit':'Artifact review only. Final CI, native upgrade/reboot/removal, and public download checks remain release conditions.'}
(OUT/'result.json').write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps(summary),flush=True)
