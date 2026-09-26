"""Replay audited defects against a separate candidate copy; restore every owner."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

source = Path('/home/user/workspace')
out = Path('/tmp/pkg-cleanup/final-mutations')
work = out / 'workspace'
out.mkdir(exist_ok=True)
assert not work.exists(), 'use a fresh mutation workspace'
names = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=source).decode().split('\0')
for name in set(names):
    p = source / name
    if name and p.is_file():
        q = work / name
        q.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(p, q)
env = os.environ | {
    'PATH': '/home/user/.cargo/bin:' + os.environ['PATH'],
    'CARGO_TARGET_DIR': '/home/user/workspace/target',
    'CARGO_BUILD_JOBS': '1',
    'CARGO_PROFILE_TEST_DEBUG': '0',
    'CARGO_PROFILE_DEV_DEBUG': '0',
    'RUSTUP_TOOLCHAIN': '1.96.1',
}

def python_test(directory, name):
    return ['python3', '-m', 'unittest', 'discover', '-s', directory, '-p', name, '-v']

def rust_test(target, name):
    return ['cargo', 'test', '--locked', '-p', 'pkg-cli', *target, name, '--', '--exact']

renderer = python_test('tools/install', 'test_render.py')
bootstrap = python_test('tools/release', 'test_install_script.py')
workflow = python_test('tools/release', 'test_workflow.py')
privacy = rust_test(['--lib'], 'commands::execute::tests::public_result_rejects_private_runtime_material_and_reserved_fields')
doctor = rust_test(['--test', 'cli'], 'completion_is_real_static_source_and_doctor_reports_verified_host_state')
policy = rust_test(['--lib'], 'commands::execute::tests::core_engine_routes_every_variant_and_preserves_global_policy')
cases = [
    ('renderer-wrong-sha256', 'tools/install/render.py', 'hashlib.file_digest(stream, "sha256").hexdigest()', '("0" * 64)', bootstrap),
    ('renderer-symlink-accepted', 'tools/install/render.py', 'path.is_symlink() or ', '', renderer),
    ('renderer-invalid-tag-accepted', 'tools/install/render.py', 'if re.fullmatch(', 'if False and re.fullmatch(', renderer),
    ('skipped-linux-proof', 'tests/linux-clean-host/run.sh', '#!/bin/sh\n', '#!/bin/sh\nexit 0 # controlled audit mutation\n', workflow),
    ('production-manual-gate-removed', '.github/workflows/release.yml', "if: ${{ github.event_name == 'workflow_dispatch' && inputs.production-linux }}", 'if: ${{ true }}', workflow),
    ('private-result-leak', 'crates/pkg-cli/src/commands/execute.rs', '        || value.contains("/nix/")\n', '', privacy),
    ('doctor-missing-checks', 'crates/pkg-cli/src/commands/doctor.rs', '            subsystem_check("runtime.managed", &inputs.managed_runtime),\n            subsystem_check("channel.signed", &inputs.channel),\n', '', doctor),
    ('state-boundary-trusts-home', 'crates/pkg-cli/src/path.rs', '.map(|user| user.dir);', '.and_then(|_| std::env::var_os("HOME").map(PathBuf::from));', rust_test(['--test', 'cli'], 'history_uses_the_system_home_boundary_despite_spoofed_or_absent_home')),
    ('renderer-swapped-artifact-digests', 'tools/install/render.py', '    source = (ROOT / "docs/install.sh").read_text()', '    replacements["PKG_SHA256_X86_64_LINUX"], replacements["PKG_SHA256_MACOS_PACKAGE"] = replacements["PKG_SHA256_MACOS_PACKAGE"], replacements["PKG_SHA256_X86_64_LINUX"]\n    source = (ROOT / "docs/install.sh").read_text()', bootstrap),
    ('gc-dry-run-policy-lost', 'crates/pkg-cli/src/commands/execute.rs', 'Command::Gc(args) => self.operations.gc(args, policy),', 'Command::Gc(args) => self.operations.gc(args, OperationPolicy { dry_run: false, ..policy }),', policy),
    ('duplicate-remove-dispatch', 'crates/pkg-cli/src/commands/execute.rs', 'Command::Remove(args) => self.operations.remove(args, policy),', 'Command::Remove(args) => { self.operations.remove(args, policy)?; self.operations.remove(args, policy) },', policy),
]

def run(label, command):
    start = time.monotonic()
    result = subprocess.run(command, cwd=work, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=420)
    (out / (label + '.log')).write_text(result.stdout)
    counts = re.findall(r'(?:test result:.*|Ran \d+ tests? in.*|FAILED \(.*\)|OK.*)', result.stdout)
    return {'command': command, 'exit_code': result.returncode, 'seconds': round(time.monotonic() - start, 2), 'summary': counts, 'log': label + '.log'}, result.stdout

results = []
for name, filename, before, after, command in cases:
    path = work / filename
    original = path.read_bytes()
    text = original.decode()
    assert text.count(before) == 1, (name, text.count(before))
    try:
        path.write_text(text.replace(before, after, 1))
        mutant, log = run(name + '-mutant', command)
        assertion_failed = ('AssertionError' in log and 'FAILED' in log) if command[0] == 'python3' else ('test result: FAILED.' in log and 'panicked at' in log)
    finally:
        path.write_bytes(original)
    assert path.read_bytes() == original
    restored, log = run(name + '-restored', command)
    executed = bool(re.search(r'(?:[1-9][0-9]* passed;|Ran [1-9][0-9]* tests?)', log))
    row = {'id': name, 'file': filename, 'before': before, 'after': after, 'mutant': mutant, 'restored': restored, 'assertion_caught_mutation': mutant['exit_code'] != 0 and assertion_failed, 'restored_passed': restored['exit_code'] == 0 and executed, 'source_restored_sha256': hashlib.sha256(original).hexdigest()}
    results.append(row)
    (out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    print(name, row['assertion_caught_mutation'], row['restored_passed'], flush=True)

assert all(row['assertion_caught_mutation'] and row['restored_passed'] for row in results), 'mutation preservation failed'
print('All controlled mutations caught; all restored controls passed.', flush=True)
