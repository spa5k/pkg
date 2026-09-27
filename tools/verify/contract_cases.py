"""Known behavior regressions whose tests must remain effective."""
import sys


def python_test(directory, name):
    return [sys.executable, '-m', 'unittest', 'discover', '-s', directory, '-p', name, '-v']


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
