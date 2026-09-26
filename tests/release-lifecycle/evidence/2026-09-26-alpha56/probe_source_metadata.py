#!/usr/bin/env python3
"""Reproduce signed alpha.56 metadata evaluation on a disposable hosted Mac."""
import json, os, pathlib, pwd, re, selectors, shutil
import signal, subprocess, sys, tempfile, time

assert sys.platform == 'darwin' and os.geteuid() == 0
assert os.environ.get('GITHUB_ACTIONS') == 'true' and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted'
repo = pathlib.Path(sys.argv[1]).resolve()
account = pwd.getpwnam('pkg-nix-broker')
uid, gid = account.pw_uid, account.pw_gid
nix = '/nix/var/nix/profiles/default/bin/nix'
source = subprocess.check_output(['/usr/bin/git', '-C', str(repo), 'show', 'v0.1.0-alpha.56:crates/pkg-nix/src/real/process.rs'], text=True)
function = source.split('fn macos_source_profile(', 1)[1]
match = re.search(r'r#"(.*?)"#', function, re.S)
assert match is not None
probe_home = tempfile.mkdtemp(prefix='pkg-nix-source-probe-', dir='/private/var/db/pkg-source')
os.chown(probe_home, uid, gid)
os.chmod(probe_home, 0o700)
binary = pathlib.Path(nix).resolve(strict=True)
profile = match.group(1)
for name, value in {'source_home': str(pathlib.Path(probe_home).resolve()), 'binary_parent': str(binary.parent), 'binary': str(binary)}.items():
    profile = profile.replace('{' + name + '}', json.dumps(value))
assert not re.search(r'\{(?:source_home|binary_parent|binary)\}', profile)
base = ['--extra-experimental-features', 'nix-command flakes', '--option', 'allow-import-from-derivation', 'false']
source_args = base + ['--option', 'accept-flake-config', 'false', '--option', 'pure-eval', 'true', '--option', 'restrict-eval', 'true', '--option', 'allow-unsafe-native-code-during-evaluation', 'false', '--option', 'use-registries', 'false', '--option', 'allowed-uris', 'https:// github: gitlab: git+https://']
args = source_args + ['--option', 'lazy-trees', 'false', 'flake', 'metadata', '--json', '--no-update-lock-file', 'github:casey/just']
buffers = {'stdout': bytearray(), 'stderr': bytearray()}
limits = {'stdout': 1024 * 1024, 'stderr': 64 * 1024}
reason = None
process = None
try:
    process = subprocess.Popen(['/usr/bin/sandbox-exec', '-p', profile, nix, *args], cwd=probe_home,
        env={'HOME': probe_home, 'TMPDIR': probe_home, 'PATH': '/usr/bin:/bin', 'NIX_USER_CONF_FILES': ''},
        user=uid, group=gid, extra_groups=os.getgrouplist(account.pw_name, gid), start_new_session=True,
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    deadline = time.monotonic() + 120
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ, 'stdout')
        selector.register(process.stderr, selectors.EVENT_READ, 'stderr')
        while selector.get_map():
            if time.monotonic() >= deadline:
                reason = 'probe_timeout'; break
            for key, _ in selector.select(0.2):
                chunk = os.read(key.fd, 65536)
                if not chunk:
                    selector.unregister(key.fileobj); continue
                name = key.data
                remaining = limits[name] - len(buffers[name])
                buffers[name].extend(chunk[:remaining])
                if len(chunk) > remaining:
                    reason = name + '_limit'; break
            if reason: break
    if reason is None:
        process.wait(timeout=max(0.1, deadline - time.monotonic()))
except subprocess.TimeoutExpired:
    reason = 'probe_timeout'
finally:
    if process is not None:
        try: os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError: pass
        process.wait(timeout=5)
    shutil.rmtree(probe_home)
print(json.dumps({'stage': 'flake_metadata', 'exit': process.returncode if process else None,
    'probeLimit': reason, 'stdout': buffers['stdout'].decode('utf-8', 'replace'),
    'stderr': buffers['stderr'].decode('utf-8', 'replace')}, indent=2))
