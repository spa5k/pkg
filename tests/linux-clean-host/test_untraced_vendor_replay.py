#!/usr/bin/env python3
import stat
import subprocess
import sys
import tempfile
import importlib.util
import os
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
CAPTURE = HERE / "pkg_bounded_capture.py"
spec = importlib.util.spec_from_file_location("pkg_bounded_capture", CAPTURE)
bounded_capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bounded_capture)
capture_source = CAPTURE.read_text()

with tempfile.TemporaryDirectory() as directory:
    root = Path(directory) / "private"
    command = [
        sys.executable,
        str(CAPTURE),
        "1024",
        str(root / "status"),
        str(root / "stdout"),
        str(root / "stderr"),
        "--",
        sys.executable,
        "-c",
        'import sys; print("o" * 900); print("e" * 900, file=sys.stderr); raise SystemExit(7)',
    ]
    result = subprocess.run(command, check=False)
    assert result.returncode == 7
    assert sum((root / name).stat().st_size for name in ("stdout", "stderr")) == 1024
    assert "exit_status=7" in (root / "status").read_text()
    assert "truncated=true" in (root / "status").read_text()
    assert stat.S_IMODE(root.stat().st_mode) == 0o700
    for name in ("status", "stdout", "stderr"):
        assert stat.S_IMODE((root / name).stat().st_mode) == 0o600

    valid = root / "valid"
    valid.write_bytes(b"started")
    valid.chmod(0o600)
    assert bounded_capture.verified_read(valid, 7, os.geteuid(), os.getegid()) == b"started"
    symlink = root / "symlink"
    symlink.symlink_to(valid)
    fifo = root / "fifo"
    os.mkfifo(fifo, 0o600)
    wrong_mode = root / "wrong-mode"
    wrong_mode.write_bytes(b"started")
    wrong_mode.chmod(0o644)
    oversize = root / "oversize"
    oversize.write_bytes(b"12345678")
    oversize.chmod(0o600)
    hardlink = root / "hardlink"
    os.link(valid, hardlink)
    for hostile, limit in ((symlink, 7), (fifo, 7), (wrong_mode, 7), (oversize, 7), (hardlink, 7)):
        try:
            bounded_capture.verified_read(hostile, limit, os.geteuid(), os.getegid())
        except ValueError:
            pass
        else:
            raise AssertionError(f"unsafe source was accepted: {hostile.name}")
        copied = subprocess.run(
            [sys.executable, str(CAPTURE), "copy", str(hostile), str(limit)],
            check=False,
            capture_output=True,
        )
        assert copied.returncode == 1 and copied.stdout == b""

run = (HERE / "run.sh").read_text()
dockerfile = (HERE / "Dockerfile").read_text()
capture = run.split("capture_failure() {", 1)[1].split("\ncleanup() {", 1)[0]
replay = run.split("capture_vendor_replay() {", 1)[1].split("\ncapture_failure() {", 1)[0]
for snapshot in (
    'docker inspect "$container"',
    'docker logs "$container"',
    'final-state.txt',
    'residue.txt',
    'handoff.json',
):
    assert capture.index(snapshot) < capture.index('capture_vendor_replay "$failure"')
assert replay.index("handoff_is_started") < replay.index('docker run')
assert 'Path("/nix").exists()' in run and 'Path("/nix").is_symlink()' in run
for descriptor_check in (
    "os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC",
    "metadata = os.fstat(descriptor)",
    "metadata.st_size == 47",
    "remaining = 47",
    "b\"\".join(chunks)",
    "os.read(descriptor, 1) == b\"\"",
):
    assert descriptor_check in run
assert 'diagnostic_container="${container}-vendor-replay"' in run
assert replay.count('docker rm --force "$diagnostic_container"') >= 1
assert "DETSYS_IDS_TELEMETRY=disabled" in replay
assert "timeout --signal=TERM --kill-after=10s 3600s" in replay
assert "HOME=/root" in replay
assert "TMPDIR=/var/lib/pkg-install/tmp" in replay
assert "--diagnostic-endpoint" in replay and "http://127.0.0.1:18080" in replay
for forbidden in ("--logger", "--log-directive", "--log-directives", "-vv", "RUST_LOG"):
    assert forbidden not in replay
production_command = """env -i \\
        HOME=/root \\
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \\
        TMPDIR=/var/lib/pkg-install/tmp \\
        DETSYS_IDS_TELEMETRY=disabled \\
        /var/lib/pkg-install/tmp/nix-installer \\
        --diagnostic-endpoint http://127.0.0.1:18080 \\
        install --determinate --no-confirm --no-modify-profile"""
assert production_command in replay
assert "74918096" in replay
assert replay.count("9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c") >= 3
assert "0:0:644:74918096" in replay
assert "0:0:700:74918096" in replay
assert 'install -m 0700 -o root -g root "$resolved" /var/lib/pkg-install/tmp/nix-installer' in replay
assert "groupadd --gid 30033 --system pkg-nix-broker" in replay
assert "useradd --uid 30033 --gid 30033 --system" in replay
assert 'stat -c %u:%g:%a /var/lib/pkg/broker-home' in replay
assert "install -d -m 0700 -o root -g root" in replay
assert "/var/lib/pkg-install /var/lib/pkg-install/tmp" in replay
assert 'test "$(stat -c %u:%g:%a /var/lib/pkg-install)" = 0:0:700' in replay
assert 'test "$(stat -c %u:%g:%a /var/lib/pkg-install/tmp)" = 0:0:700' in replay
assert 'pkg_bounded_capture.py 1048576' in replay
assert 'pkg_bounded_capture.py 262144' in replay
assert "timeout --signal=TERM --kill-after=5s 30s" in replay
assert "stdin=subprocess.DEVNULL" in capture_source
assert "head -c" not in capture
assert "head -c" not in replay
assert "/usr/local/libexec/pkg_bounded_capture.py" in replay
assert 'copy "$source" "$limit"' in run
assert 'capture_vendor_replay "$failure" || true' in capture
assert run.count('capture_vendor_replay "$failure"') == 1
assert capture.index("transaction-journal.json") < capture.index('capture_vendor_replay "$failure"')
assert "/var/lib/pkg-install-journal/transaction-v1.json" in capture
assert "/run/pkg-install/transaction-v1.json" not in run
assert "65536" in capture
assert "broker-acquisition.txt" in capture
assert "pkg_bounded_capture.py 4096" in capture
assert "--unit=pkg-nix-broker.service" in capture
assert "--lines=1" in capture
assert "source|fetch|resolve|preflight|probe|progress|verification|evidence" in capture
assert "adapter_failure|unapproved_signature|integrity_failure|trust_failure|metadata_mismatch" in capture
assert "validation_failure|timeout|unavailable|trust_failure|integrity_failure" in capture
assert "permission_denied|operation_failed" in capture
assert capture.index("pkg_bounded_capture.py 4096") < capture.index("broker-acquisition.txt")
assert "def verified_read(path, limit, expected_uid=0, expected_gid=0)" in capture_source
for metadata_check in (
    "stat.S_ISREG(metadata.st_mode)", "metadata.st_uid != expected_uid",
    "metadata.st_gid != expected_gid", "stat.S_IMODE(metadata.st_mode) != 0o600",
    "metadata.st_nlink != 1", "metadata.st_size > limit",
):
    assert metadata_check in capture_source
for product_path in (
    "/opt/pkg", "/opt/pkg/etc", "/opt/pkg/etc/pkg", "/opt/pkg/uninstall", "/opt/pkg/bin",
    "/var/lib/pkg", "/var/lib/pkg/log", "/var/lib/pkg/log/broker",
    "/var/lib/pkg/log/helper", "/var/lib/pkg/broker-home",
    "/var/lib/pkg/broker-home/channel", "/var/lib/pkg/broker-home/tmp",
    "/var/lib/pkg/helper-home", "/var/lib/pkg/helper-home/tmp", "/run/pkg-helper", "/run/pkg",
):
    assert product_path in replay
assert "/opt/pkg/etc/pkg/nix.conf" not in replay
assert "product_prestate=partial-non-file-metadata-only" in replay
for omission in ("nix_config", "handoff", "journal", "channel_contents"):
    assert f"{omission}=omitted" in replay
bootstrap = run.split('echo "+ authenticated ownership drift refusal"', 1)[1].split("stop_container", 1)[0]
assert "/run/pkg-bootstrap-capture/status.txt" in bootstrap
assert "/run/pkg-bootstrap-capture/stdout" in bootstrap
assert "/run/pkg-bootstrap-capture/stderr" in bootstrap
assert bootstrap.index("pkg_bounded_capture.py 262144") < bootstrap.index("/usr/local/sbin/pkg-bootstrap")
assert ">/dev/null 2>&1" in bootstrap
assert "|| true" not in bootstrap and "|| :" not in bootstrap
assert 'bootstrap=$failure/bootstrap' in capture
cleanup = run.split("cleanup() {", 1)[1].split("\n}", 1)[0]
assert cleanup.index("trap 'cleanup_after_signal 130' INT") < cleanup.index('capture_failure "$status"')
assert cleanup.index("trap 'cleanup_after_signal 143' TERM") < cleanup.index('capture_failure "$status"')
assert '0|130|143) ;;' in cleanup
signal_cleanup = run.split("cleanup_after_signal() {", 1)[1].split("\n}", 1)[0]
assert "trap '' INT TERM" in signal_cleanup
assert signal_cleanup.index("stop_container") < signal_cleanup.index('rm -rf "$stage_root"')
assert signal_cleanup.index('rm -rf "$stage_root"') < signal_cleanup.index('exit "$signal_status"')
ready = run.split("wait_container_ready() {", 1)[1].split("\n}", 1)[0]
assert 'docker logs "$target_container"' in ready
assert "return 1" in ready
assert "exit 1" not in ready
offline = run.split("assert_product_units_offline() {", 1)[1].split("\n}", 1)[0]
assert 'case "$unit" in' in offline
assert '*.service) test "$(docker exec "$container" systemctl show --property=MainPID --value "$unit")" = 0 ;;' in offline
assert offline.count("--property=MainPID") == 1
publication_product = run.split("assert_publication_product() {", 1)[1].split("\n}", 1)[0]
assert 'docker exec -i "$container" python3 - "$publication"' in publication_product
publication_product_full = run.split("assert_publication_product() {", 1)[1].split("\nsnapshot_package_state()", 1)[0]
assert 'return f"sha256-{hex_digest}"' in publication_product_full
assert 'receipt["ownershipManifestDigest"] != receipt_digest(descriptor["sha256"])' in publication_product_full
assert 'records[asset]["contentDigest"] != receipt_digest(expected[target])' in publication_product_full
assert 'records[asset].get("contentDigest") != receipt_digest(actual)' in publication_product_full
publication = run.split("publication_installer() {", 1)[1].split("\n}", 1)[0]
assert 'docker exec -i "$container" python3 - "$1"' in publication
proof_check = 'python3 "$repo/tests/linux-clean-host/test_untraced_vendor_replay.py" >/dev/null'
assert run.index(proof_check) < run.index('echo "+ stage x86_64 Linux release inputs"')
assert run.index("package_alpha_candidate.py") < run.index('"$repo/tests/linux-clean-host/pkg_bounded_capture.py"')
assert "COPY pkg_bounded_capture.py /usr/local/libexec/" in dockerfile
for shipping_path in (ROOT / "crates", ROOT / "docs" / "install.sh"):
    paths = shipping_path.rglob("*") if shipping_path.is_dir() else (shipping_path,)
    for path in paths:
        if path.is_file():
            assert "pkg_bounded_capture" not in path.read_text(errors="ignore")

print("bounded fresh-container untraced vendor replay gating passed")
