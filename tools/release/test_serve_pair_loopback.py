"""Focused checks for the in-VM loopback proof channel server."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import unittest
import urllib.error
import urllib.request


TOOLS = Path(__file__).resolve().parent
TOOL = TOOLS / "serve_pair_loopback.py"
PRODUCT_COMMIT = "cbd3494443b94283430d8a48e9fec65699d0210a"


def write_json(path: Path, value: dict) -> bytes:
    raw = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    path.write_bytes(raw)
    return raw


def pair(root: Path, *, product_commit: str = PRODUCT_COMMIT) -> Path:
    trusted = "b" * 64
    channels = []
    for name, release, version in (("n", "proof-n", 1), ("n-plus-1", "proof-n-plus-1", 2)):
        channel = root / name
        required = [
            "metadata/1.root.json", f"metadata/{version}.targets.json",
            f"metadata/{version}.snapshot.json", "metadata/timestamp.json",
        ]
        for relative in (*required, "targets/payload"):
            path = channel / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"{name}:{relative}\n".encode())
        write_json(channel / "release-manifest.json", {
            "schemaVersion": 2, "releaseId": release, "channelSequence": version,
            "timestampVersion": version, "trustedRootSha256": trusted,
        })
        files = []
        for path in sorted(path for path in channel.rglob("*") if path.is_file()):
            raw = path.read_bytes()
            files.append({
                "path": path.relative_to(channel).as_posix(),
                "sha256": hashlib.sha256(raw).hexdigest(), "length": len(raw),
            })
        inventory_name = f"{name}.inventory.json"
        inventory = write_json(root / inventory_name, {"schemaVersion": 1, "files": files})
        channels.append({
            "name": name, "releaseId": release, "manifestSchemaVersion": 2,
            "channelSequence": version, "timestampVersion": version,
            "trustedRootSha256": trusted, "inventory": inventory_name,
            "inventorySha256": hashlib.sha256(inventory).hexdigest(),
            "inventoryLength": len(inventory), "requiredMetadataPaths": required,
            "requiredTargetPrefix": "targets/",
        })
    write_json(root / "proof-pair.json", {
        "schemaVersion": 1, "channels": channels, "productCommit": product_commit,
    })
    return root


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def run(*arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-I", str(TOOL), *arguments],
        capture_output=True, text=True, timeout=120,
    )


def read_records(state: Path) -> dict[str, tuple[str, int]]:
    sys.path.insert(0, str(TOOLS))
    import serve_pair_loopback as server  # noqa: PLC0415

    return server.validate_pair(state)


class LoopbackServerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workspace = Path(tempfile.mkdtemp(prefix="pkg-dn1-loopback-test-"))

    def tearDown(self) -> None:
        for descriptor in sorted(self.workspace.glob("*/state.json")):
            if run("stop", str(descriptor.parent)).returncode != 0:
                state = json.loads(descriptor.read_bytes())
                if state.get("http_pid"):
                    try:
                        os.killpg(state["http_pid"], signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        pass
        for path in self.workspace.rglob("*"):
            if path.is_dir():
                path.chmod(0o700)
            else:
                path.chmod(0o600)
        self.workspace.chmod(0o700)
        shutil.rmtree(self.workspace, ignore_errors=True)

    def stage(self, name: str = "staging", **pair_arguments) -> Path:
        staging = self.workspace / name
        staging.mkdir(mode=0o700)
        pair(staging, **pair_arguments)
        return staging

    def bootstrap(self, staging: Path, port: int) -> subprocess.CompletedProcess[str]:
        return run(
            "bootstrap", str(staging), str(self.workspace / "channel"),
            str(self.workspace / "state"), str(port),
        )

    def ca_context(self, state: Path) -> ssl.SSLContext:
        context = ssl.create_default_context(cafile=str(state / "ca.pem"))
        context.check_hostname = True
        return context

    def test_bootstrap_serves_the_verified_pair_over_loopback_tls(self) -> None:
        staging = self.stage()
        port = free_port()
        result = self.bootstrap(staging, port)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"url=https://127.0.0.1:{port}", result.stdout)
        state = self.workspace / "state"
        records = read_records(self.workspace / "channel")
        self.assertIn(f"verified_files={len(records)}", result.stdout)
        self.assertIn(
            f"verified_bytes={sum(expected[1] for expected in records.values())}",
            result.stdout,
        )
        status = run("status", str(state))
        self.assertEqual(status.returncode, 0, status.stderr)

        context = self.ca_context(state)
        for relative, expected in sorted(records.items()):
            with urllib.request.urlopen(
                f"https://127.0.0.1:{port}/{relative}", timeout=10, context=context
            ) as response:
                raw = response.read()
            self.assertEqual(hashlib.sha256(raw).hexdigest(), expected[0])
            self.assertEqual(len(raw), expected[1])
        with urllib.request.urlopen(
            f"https://127.0.0.1:{port}/n/metadata/timestamp.json",
            timeout=10, context=context,
        ) as response:
            self.assertEqual(response.status, 200)

    def test_served_channel_is_read_only_and_exactly_the_pair(self) -> None:
        staging = self.stage()
        port = free_port()
        result = self.bootstrap(staging, port)
        self.assertEqual(result.returncode, 0, result.stderr)
        channel = self.workspace / "channel"
        self.assertEqual(
            {path.name for path in channel.iterdir()},
            {"n", "n-plus-1", "n.inventory.json", "n-plus-1.inventory.json", "proof-pair.json"},
        )
        self.assertFalse(any(path.is_symlink() for path in channel.rglob("*")))
        self.assertTrue(all(not path.stat().st_mode & 0o222 for path in channel.rglob("*")))
        self.assertFalse(staging.exists())

    def test_plain_http_and_untrusted_tls_are_refused(self) -> None:
        staging = self.stage()
        port = free_port()
        result = self.bootstrap(staging, port)
        self.assertEqual(result.returncode, 0, result.stderr)

        with self.assertRaises(OSError):
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/proof-pair.json", timeout=10):
                pass
        with self.assertRaises(urllib.error.URLError):
            with urllib.request.urlopen(
                f"https://127.0.0.1:{port}/proof-pair.json", timeout=10
            ):
                pass

    def test_stop_removes_the_publication_state_and_port(self) -> None:
        staging = self.stage()
        port = free_port()
        result = self.bootstrap(staging, port)
        self.assertEqual(result.returncode, 0, result.stderr)
        stop = run("stop", str(self.workspace / "state"))
        self.assertEqual(stop.returncode, 0, stop.stderr)
        self.assertFalse((self.workspace / "channel").exists())
        self.assertFalse((self.workspace / "state").exists())
        with socket.socket() as probe:
            probe.settimeout(1)
            self.assertNotEqual(probe.connect_ex(("127.0.0.1", port)), 0)
        self.assertGreater(run("status", str(self.workspace / "state")).returncode, 0)

    def test_bootstrap_restores_a_rejected_staging_directory(self) -> None:
        staging = self.stage()
        (staging / "extra").write_bytes(b"unexpected\n")
        port = free_port()
        result = self.bootstrap(staging, port)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected top-level entries", result.stderr)
        self.assertTrue(staging.exists())
        self.assertFalse((self.workspace / "channel").exists())
        self.assertFalse((self.workspace / "state").exists())

    def test_bootstrap_restores_staging_when_a_pair_file_is_tampered(self) -> None:
        staging = self.stage()
        target = staging / "n" / "targets" / "payload"
        raw = target.read_bytes()
        target.write_bytes(raw + b"tampered\n")
        port = free_port()
        result = self.bootstrap(staging, port)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match its inventory", result.stderr)
        self.assertEqual(target.read_bytes(), raw + b"tampered\n")
        self.assertFalse((self.workspace / "state").exists())

    def test_bootstrap_refuses_an_unsafe_layout(self) -> None:
        staging = self.stage("elsewhere")
        port = free_port()
        nested = self.workspace / "nested"
        nested.mkdir(mode=0o700)
        result = run(
            "bootstrap", str(staging), str(nested / "channel"),
            str(self.workspace / "state"), str(port),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("sibling", result.stderr)

        again = self.stage("staging")
        first = self.bootstrap(again, port)
        self.assertEqual(first.returncode, 0, first.stderr)
        duplicate = self.stage("staging-2")
        result = self.bootstrap(duplicate, free_port())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("publication", result.stderr)
        self.assertTrue(duplicate.exists())

    def test_bootstrap_refuses_ports_outside_the_unprivileged_range(self) -> None:
        staging = self.stage()
        result = self.bootstrap(staging, 80)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unprivileged", result.stderr)

    def test_stop_refuses_a_foreign_process_in_the_state(self) -> None:
        staging = self.stage()
        port = free_port()
        result = self.bootstrap(staging, port)
        self.assertEqual(result.returncode, 0, result.stderr)
        state_path = self.workspace / "state" / "state.json"
        state = json.loads(state_path.read_bytes())
        original_pid = state["http_pid"]
        foreign = subprocess.Popen(
            [sys.executable, "-I", "-c", "import time; time.sleep(60)"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
        )
        try:
            state["http_pid"] = foreign.pid
            raw = (json.dumps(state, sort_keys=True, separators=(",", ":")) + "\n").encode()
            state_path.write_bytes(raw)
            refused = run("stop", str(self.workspace / "state"))
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn("foreign process", refused.stderr)
            self.assertTrue(foreign.poll() is None)
        finally:
            foreign.terminate()
            foreign.wait(timeout=10)
            state["http_pid"] = original_pid
            restored = (json.dumps(state, sort_keys=True, separators=(",", ":")) + "\n").encode()
            state_path.write_bytes(restored)

    def test_status_fails_closed_on_a_corrupt_state(self) -> None:
        staging = self.stage()
        port = free_port()
        result = self.bootstrap(staging, port)
        self.assertEqual(result.returncode, 0, result.stderr)
        state_path = self.workspace / "state" / "state.json"
        valid = state_path.read_bytes()
        try:
            state_path.write_bytes(b"{")
            self.assertGreater(run("status", str(self.workspace / "state")).returncode, 0)
        finally:
            state_path.write_bytes(valid)

    def test_usage_is_bounded_to_the_documented_commands(self) -> None:
        for arguments in ([], ["status"], ["frobnicate"], ["bootstrap", "a", "b", "c"]):
            self.assertGreater(run(*arguments).returncode, 0)


if __name__ == "__main__":
    unittest.main()
