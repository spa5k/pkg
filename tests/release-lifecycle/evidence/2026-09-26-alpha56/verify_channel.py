import datetime, hashlib, json, sys
from pathlib import Path
import subprocess, tempfile

root_path=Path('/Users/pacific/.local/share/pkg-release/test/root.json')
assert hashlib.sha256(root_path.read_bytes()).hexdigest()=='2f54413cd26fd913067c6aceba21191b45bad0122296669c647009765b6787cc'
root=json.loads(root_path.read_bytes())
channel=Path(sys.argv[1]); sequence=int(sys.argv[2])
assert (channel/'metadata/1.root.json').read_bytes()==root_path.read_bytes()

def load(role, path):
    doc=json.loads(path.read_bytes())
    payload=json.dumps(doc['signed'],sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()
    allowed=root['signed']['roles'][role]
    verified=set()
    for sig in doc['signatures']:
        keyid=sig['keyid']
        if keyid not in allowed['keyids']: continue
        key=root['signed']['keys'][keyid]
        assert key['keytype']==key['scheme']=='ed25519'
        with tempfile.TemporaryDirectory() as temporary:
            tmp=Path(temporary)
            (tmp/'key.der').write_bytes(bytes.fromhex('302a300506032b6570032100'+key['keyval']['public']))
            (tmp/'signature').write_bytes(bytes.fromhex(sig['sig']))
            (tmp/'payload').write_bytes(payload)
            subprocess.run(['/opt/homebrew/opt/openssl@3/bin/openssl','pkeyutl','-verify','-rawin','-pubin','-keyform','DER','-inkey',str(tmp/'key.der'),'-sigfile',str(tmp/'signature'),'-in',str(tmp/'payload')],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        verified.add(keyid)
    assert len(verified)>=allowed['threshold']
    assert datetime.datetime.fromisoformat(doc['signed']['expires'].replace('Z','+00:00'))>datetime.datetime.now(datetime.timezone.utc)
    return doc['signed']

def check(path, record):
    data=path.read_bytes()
    assert len(data)==record['length'], path
    assert hashlib.sha256(data).hexdigest()==record['hashes']['sha256'],path
    return data

load('root',root_path)
timestamp=load('timestamp',channel/'metadata/timestamp.json')
assert timestamp['version']==sequence
snapshot_name=f'{sequence}.snapshot.json'; snapshot_path=channel/'metadata'/snapshot_name
assert timestamp['meta']['snapshot.json']['version']==sequence
check(snapshot_path,timestamp['meta']['snapshot.json'])
snapshot=load('snapshot',snapshot_path)
targets_path=channel/'metadata'/f'{sequence}.targets.json'
assert snapshot['version']==sequence and snapshot['meta']['targets.json']['version']==sequence
check(targets_path,snapshot['meta']['targets.json'])
targets=load('targets',targets_path)
assert targets['version']==sequence
for name, record in targets['targets'].items():
    assert not name.startswith('/') and '..' not in Path(name).parts
    check(channel/'targets'/f"{record['hashes']['sha256']}.{name}",record)
print(json.dumps({'sequence':sequence,'targets':len(targets['targets']),'timestampExpires':timestamp['expires'],'verified':True}))
