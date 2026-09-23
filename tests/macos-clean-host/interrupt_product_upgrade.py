#!/usr/bin/env python3
"""Kill a product upgrade after a durable replacement, then retry it in a VM.

Requires an older, working pkg installation and its accepted Nix receipt.
Never use this for a fresh Nix installation. See PUBLIC-04.md.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('installer', type=Path)
    parser.add_argument('evidence', type=Path)
    args = parser.parse_args()
    if os.geteuid() != 0 or not subprocess.check_output(
        ['/usr/sbin/sysctl', '-n', 'hw.model'], text=True
    ).startswith('VirtualMac'):
        parser.error('requires root inside a disposable macOS VM')
    handoff = json.loads(Path('/private/var/db/pkg-install/determinate-handoff-v1.json').read_text())
    if handoff['state']['kind'] != 'accepted':
        parser.error('Base Nix must already be accepted; never interrupt its installer')
    args.evidence.mkdir(mode=0o700)
    journal = Path('/private/var/db/pkg-install-journal/macos-transaction-v1.json')
    env = {'HOME': '/var/root', 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'PKG_INSTALL_DEBUG': '1'}
    with (args.evidence / 'interrupted.log').open('wb') as log:
        child = subprocess.Popen([str(args.installer.resolve())], env=env,
                                 stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.monotonic() + 180
        interrupted = False
        while child.poll() is None and time.monotonic() < deadline:
            try:
                state = json.loads(journal.read_text())
                if state['mode'] == 'offlineUpgrade' and not state['committed'] and any(
                    entry['state'] == 'replaced' for entry in state['entries']
                ):
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()
                    saved = journal.read_text()
                    assert not json.loads(saved)['committed']
                    (args.evidence / 'journal-at-kill.json').write_text(saved)
                    interrupted = True
                    break
            except (FileNotFoundError, json.JSONDecodeError):
                pass
            time.sleep(.001)
        if not interrupted:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
            raise RuntimeError('required replacement point was not observed; keep the logs')
    print('PASS: interrupted after a recorded replacement, before commit', flush=True)
    with (args.evidence / 'retry.log').open('wb') as log:
        subprocess.run([str(args.installer.resolve())], env=env,
                       stdout=log, stderr=subprocess.STDOUT, check=True)
    assert not journal.exists()
    for label in ['org.pkg.root-helper', 'org.pkg.nix-broker']:
        output = subprocess.check_output(['/bin/launchctl', 'print', 'system/' + label], text=True)
        assert 'state = running' in output
    print('PASS: matching installer recovered and started both services')


if __name__ == '__main__':
    main()
