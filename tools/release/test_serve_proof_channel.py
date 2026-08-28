"""Focused checks for the one-use DN-16 proof server."""

import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
import urllib.error
import urllib.request


TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))
import serve_proof_channel as server  # noqa: E402


def write_json(path: Path, value: dict) -> bytes:
    raw = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    path.write_bytes(raw)
    return raw


def pair(root: Path) -> Path:
    trusted = "a" * 64
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
    write_json(root / "proof-pair.json", {"schemaVersion": 1, "channels": channels})
    return root


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait(url: str) -> None:
    for _ in range(40):
        try:
            urllib.request.urlopen(url, timeout=0.2).close()
            return
        except (OSError, urllib.error.URLError):
            time.sleep(0.05)
    raise AssertionError("server did not start")


def processes(root: Path, served: Path) -> tuple[Path, subprocess.Popen, subprocess.Popen]:
    state = root / "state"
    state.mkdir(mode=0o700)
    port = free_port()
    cloudflared = root / "cloudflared"
    cloudflared.write_text("#!/bin/sh\nwhile :; do sleep 1; done\n")
    cloudflared.chmod(0o700)
    http = subprocess.Popen(
        [sys.executable, str(server.__file__), "_serve", str(served), str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
    )
    tunnel = subprocess.Popen(
        [str(cloudflared), "tunnel", "--url", f"http://127.0.0.1:{port}"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
    )
    for name in ("http.log", "cloudflared.log"):
        (state / name).write_bytes(b"")
    write_json(state / "state.json", {
        "version": 1, "phase": "bootstrap", "publication": str(served), "port": port,
        "url": "https://proof.trycloudflare.com", "cloudflared": str(cloudflared),
        "http_pid": http.pid, "cloudflared_pid": tunnel.pid,
    })
    wait(f"http://127.0.0.1:{port}/")
    return state, http, tunnel


def kill(processes: tuple[subprocess.Popen, ...]) -> None:
    for process in processes:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)


class ProofServerTests(unittest.TestCase):
    def test_inventory_covers_every_file_and_refuses_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pair(Path(directory))
            records = server.validate_pair(root)
            self.assertEqual(set(records), {
                "proof-pair.json", "n.inventory.json", "n-plus-1.inventory.json",
                *(f"n/{item['path']}" for item in json.loads((root / "n.inventory.json").read_text())["files"]),
                *(f"n-plus-1/{item['path']}" for item in json.loads((root / "n-plus-1.inventory.json").read_text())["files"]),
            })
            (root / "n/foreign").symlink_to(root / "proof-pair.json")
            with self.assertRaisesRegex(ValueError, "symlink"):
                server.validate_pair(root)
        with self.assertRaisesRegex(ValueError, "repository root"):
            server.validate_pair(server.REPO)

    def test_remote_verification_checks_every_inventoried_byte(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pair(Path(directory))
            records = server.validate_pair(root)
            server.make_read_only(root)
            port = free_port()
            process = subprocess.Popen(
                [sys.executable, str(server.__file__), "_serve", str(root), str(port)],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
            )
            try:
                wait(f"http://127.0.0.1:{port}/")
                server.verify_remote(f"http://127.0.0.1:{port}", records)
                changed = records.copy()
                changed["n/targets/payload"] = ("0" * 64, changed["n/targets/payload"][1])
                with self.assertRaisesRegex(ValueError, "differs"):
                    server.verify_remote(f"http://127.0.0.1:{port}", changed)
            finally:
                kill((process,))

    def test_failed_remote_verification_rolls_back_and_retry_succeeds(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = pair(root / "source")
            served = root / "served"
            served.mkdir(mode=0o555)
            state, http, tunnel = processes(root, served)
            try:
                with mock.patch.object(server, "verify_remote", side_effect=ValueError("remote")):
                    with self.assertRaisesRegex(ValueError, "remote"):
                        server.activate(str(source), str(state))
                self.assertFalse(any(served.iterdir()))
                self.assertEqual(server.read_state(state)["phase"], "bootstrap")

                with mock.patch.object(server, "verify_remote") as verify:
                    server.activate(str(source), str(state))
                self.assertEqual(server.read_state(state)["phase"], "active")
                self.assertTrue(all(path.stat().st_mode & 0o222 == 0 for path in (served, *served.rglob("*"))))
                verify.assert_called_once()
                self.assertEqual(server.status(str(state)), 0)
                server.stop(str(state))
                self.assertFalse(state.exists())
                self.assertFalse(served.exists())
            finally:
                kill((http, tunnel))

    def test_old_start_and_repeat_activation_are_refused(self) -> None:
        with self.assertRaisesRegex(ValueError, "usage"):
            server.main(["start", "/tmp/publication", "/tmp/state", "8080"])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = pair(root / "source")
            served = root / "served"
            served.mkdir(mode=0o555)
            state, http, tunnel = processes(root, served)
            try:
                with mock.patch.object(server, "verify_remote"):
                    server.activate(str(source), str(state))
                with self.assertRaisesRegex(ValueError, "not at bootstrap"):
                    server.activate(str(source), str(state))
            finally:
                kill((http, tunnel))


if __name__ == "__main__":
    unittest.main()
