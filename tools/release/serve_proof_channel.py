#!/usr/bin/env python3
"""Expose one immutable DN-16 N/N+1 proof pair through a Quick Tunnel."""

import hashlib
import http.server
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


REPO = Path(__file__).resolve().parents[2]
PAIR_FILES = {"n", "n-plus-1", "n.inventory.json", "n-plus-1.inventory.json", "proof-pair.json"}
URL = re.compile(r"https://[a-z0-9-]+\.trycloudflare\.com")
STATE_FILES = ("state.json", "http.log", "cloudflared.log")
CLOUDFLARED_ARGUMENTS = ("tunnel", "--config", "/dev/null", "--url")


def fail(message: str) -> ValueError:
    return ValueError(message)


def directory(value: str, *, exists: bool) -> Path:
    supplied = Path(value)
    if not supplied.is_absolute() or supplied.is_symlink():
        raise fail("directory must be an absolute non-symlink path")
    root = supplied.resolve(strict=exists)
    if root == REPO:
        raise fail("refusing the repository root")
    return root


def private_state(value: str, *, create: bool = False) -> Path:
    root = directory(value, exists=not create)
    if create:
        if root.exists():
            raise fail("state directory already exists")
        root.mkdir(mode=0o700)
        root.chmod(0o700)
    metadata = root.stat()
    if not root.is_dir() or metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise fail("invalid state directory")
    return root


def load_json(path: Path, maximum: int = 4 * 1024 * 1024) -> tuple[dict, bytes]:
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or path.is_symlink() or metadata.st_size > maximum:
        raise fail(f"invalid JSON file: {path.name}")
    raw = path.read_bytes()

    def unique(pairs: list[tuple[str, object]]) -> dict:
        value = dict(pairs)
        if len(value) != len(pairs):
            raise fail(f"duplicate JSON key: {path.name}")
        return value

    value = json.loads(raw, object_pairs_hook=unique)
    if not isinstance(value, dict):
        raise fail(f"invalid JSON object: {path.name}")
    return value, raw


def digest(path: Path) -> tuple[str, int]:
    value = hashlib.sha256()
    length = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            value.update(chunk)
            length += len(chunk)
    return value.hexdigest(), length


def safe_relative(value: object) -> str:
    if not isinstance(value, str):
        raise fail("inventory path is not text")
    path = PurePosixPath(value)
    if path.is_absolute() or not path.parts or any(part in ("", ".", "..") for part in path.parts):
        raise fail("inventory path is unsafe")
    return value


def validate_pair(root: Path) -> dict[str, tuple[str, int]]:
    if root.resolve() == REPO:
        raise fail("refusing the repository root")
    metadata = root.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or root.is_symlink() or {path.name for path in root.iterdir()} != PAIR_FILES:
        raise fail("proof pair has unexpected top-level entries")
    if any(path.is_symlink() or not (path.is_dir() or path.is_file()) for path in root.rglob("*")):
        raise fail("proof pair contains a symlink or special file")
    descriptor, descriptor_raw = load_json(root / "proof-pair.json", 64 * 1024)
    if set(descriptor) != {"schemaVersion", "channels"} or descriptor["schemaVersion"] != 1:
        raise fail("invalid proof pair descriptor")
    channels = descriptor["channels"]
    if not isinstance(channels, list) or len(channels) != 2:
        raise fail("proof pair must contain two channels")

    records = {"proof-pair.json": (hashlib.sha256(descriptor_raw).hexdigest(), len(descriptor_raw))}
    trusted_root = None
    for channel, name, version in zip(channels, ("n", "n-plus-1"), (1, 2), strict=True):
        expected_keys = {
            "name", "releaseId", "manifestSchemaVersion", "channelSequence", "timestampVersion",
            "trustedRootSha256", "inventory", "inventorySha256", "inventoryLength",
            "requiredMetadataPaths", "requiredTargetPrefix",
        }
        inventory_name = f"{name}.inventory.json"
        required = [
            "metadata/1.root.json", f"metadata/{version}.targets.json",
            f"metadata/{version}.snapshot.json", "metadata/timestamp.json",
        ]
        if (
            not isinstance(channel, dict)
            or set(channel) != expected_keys
            or channel["name"] != name
            or channel["manifestSchemaVersion"] != 2
            or channel["channelSequence"] != version
            or channel["timestampVersion"] != version
            or channel["inventory"] != inventory_name
            or channel["requiredMetadataPaths"] != required
            or channel["requiredTargetPrefix"] != "targets/"
            or not isinstance(channel["releaseId"], str)
            or not re.fullmatch(r"[0-9a-f]{64}", channel["trustedRootSha256"])
        ):
            raise fail("invalid proof channel descriptor")
        if trusted_root not in (None, channel["trustedRootSha256"]):
            raise fail("proof channels use different trusted roots")
        trusted_root = channel["trustedRootSha256"]

        inventory, inventory_raw = load_json(root / inventory_name)
        if (
            set(inventory) != {"schemaVersion", "files"}
            or inventory["schemaVersion"] != 1
            or hashlib.sha256(inventory_raw).hexdigest() != channel["inventorySha256"]
            or len(inventory_raw) != channel["inventoryLength"]
            or not isinstance(inventory["files"], list)
        ):
            raise fail("invalid proof inventory")
        listed = {}
        for item in inventory["files"]:
            if not isinstance(item, dict) or set(item) != {"path", "sha256", "length"}:
                raise fail("invalid proof inventory entry")
            relative = safe_relative(item["path"])
            if (
                relative in listed
                or not re.fullmatch(r"[0-9a-f]{64}", item["sha256"])
                or not isinstance(item["length"], int)
                or item["length"] < 0
            ):
                raise fail("invalid proof inventory entry")
            listed[relative] = (item["sha256"], item["length"])
        if list(listed) != sorted(listed):
            raise fail("proof inventory is not canonical")
        channel_root = root / name
        actual = {
            path.relative_to(channel_root).as_posix()
            for path in channel_root.rglob("*")
            if path.is_file()
        }
        if actual != set(listed) or not all(path in listed for path in required):
            raise fail("proof inventory does not cover the channel")
        if not any(path.startswith("targets/") for path in listed):
            raise fail("proof channel has no targets")
        for relative, expected in listed.items():
            if digest(channel_root / relative) != expected:
                raise fail("proof file does not match its inventory")
            records[f"{name}/{relative}"] = expected
        manifest, _ = load_json(channel_root / "release-manifest.json")
        if (
            manifest.get("schemaVersion") != 2
            or manifest.get("releaseId") != channel["releaseId"]
            or manifest.get("channelSequence") != version
            or manifest.get("timestampVersion") != version
            or manifest.get("trustedRootSha256") != trusted_root
        ):
            raise fail("release manifest does not match the proof pair")
        records[inventory_name] = (channel["inventorySha256"], channel["inventoryLength"])
    return records


def owned_process(state: dict, role: str) -> tuple[int, bool]:
    pid = state[f"{role}_pid"]
    markers = (
        (str(Path(__file__).resolve()), "_serve", str(state["publication"]))
        if role == "http"
        else (
            state["cloudflared"],
            *CLOUDFLARED_ARGUMENTS,
            f"http://127.0.0.1:{state['port']}",
        )
    )
    if not isinstance(pid, int) or pid <= 1:
        raise fail("invalid process id")
    result = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True)
    alive = result.returncode == 0 and bool(result.stdout.strip())
    return pid, alive and all(marker in result.stdout for marker in markers)


def read_state(root: Path) -> dict:
    state, _ = load_json(root / "state.json", 4096)
    expected = {
        "version", "phase", "publication", "port", "url", "cloudflared", "http_pid",
        "cloudflared_pid",
    }
    if (
        set(state) != expected or state["version"] != 1 or state["phase"] not in ("bootstrap", "active")
        or not isinstance(state["url"], str) or URL.fullmatch(state["url"]) is None
        or not isinstance(state["publication"], str) or not isinstance(state["cloudflared"], str)
        or not isinstance(state["port"], int) or not 1024 <= state["port"] <= 65535
    ):
        raise fail("invalid proof server state")
    return state


def wait_empty(url: str, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"{url}/", timeout=2) as response:
                if response.status == 200:
                    return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.25)
    raise fail("empty proof origin did not become reachable")


def terminate(process: subprocess.Popen | None) -> None:
    if process is None or process.poll() is not None:
        return
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)


def write_state(root: Path, state: dict, *, replace: bool = False) -> None:
    path = root / ("state.json.new" if replace else "state.json")
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(state, stream, sort_keys=True, separators=(",", ":"))
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    if replace:
        os.replace(path, root / "state.json")


def bootstrap(publication_value: str, state_value: str, port_value: str) -> None:
    port = int(port_value)
    cloudflared = shutil.which("cloudflared")
    publication = directory(publication_value, exists=False)
    state_candidate = directory(state_value, exists=False)
    if (
        not 1024 <= port <= 65535
        or cloudflared is None
        or publication.exists()
        or state_candidate.is_relative_to(publication)
    ):
        raise fail("invalid bootstrap input")
    publication.mkdir(mode=0o555)
    publication.chmod(0o555)
    state = None
    http = tunnel = None
    try:
        state = private_state(state_value, create=True)
        http_log = (state / "http.log").open("xb")
        http = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), "_serve", str(publication), str(port)],
            stdout=http_log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        http_log.close()
        wait_empty(f"http://127.0.0.1:{port}", 10)
        tunnel_log = (state / "cloudflared.log").open("xb")
        tunnel = subprocess.Popen(
            [cloudflared, *CLOUDFLARED_ARGUMENTS, f"http://127.0.0.1:{port}"],
            stdout=tunnel_log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        tunnel_log.close()
        deadline = time.monotonic() + 60
        url = None
        while time.monotonic() < deadline and tunnel.poll() is None:
            with (state / "cloudflared.log").open("rb") as log:
                log.seek(max(0, log.seek(0, os.SEEK_END) - 65536))
                match = URL.search(log.read().decode(errors="replace"))
            if match:
                url = match.group(0)
                break
            time.sleep(0.25)
        if url is None:
            raise fail("cloudflared did not produce a Quick Tunnel URL")
        wait_empty(url, 60)
        write_state(state, {
            "version": 1, "phase": "bootstrap", "publication": str(publication), "port": port,
            "url": url, "cloudflared": cloudflared, "http_pid": http.pid,
            "cloudflared_pid": tunnel.pid,
        })
        print(url)
    except BaseException:
        terminate(tunnel)
        terminate(http)
        if state is not None:
            shutil.rmtree(state)
        publication.rmdir()
        raise


def make_read_only(root: Path) -> None:
    for path in root.rglob("*"):
        path.chmod(0o555 if path.is_dir() else 0o444)
    root.chmod(0o555)


def remove_tree(root: Path) -> None:
    for path in root.rglob("*"):
        if path.is_symlink():
            raise fail("refusing to remove a symlinked tree")
        path.chmod(0o700 if path.is_dir() else 0o600)
    root.chmod(0o700)
    shutil.rmtree(root)


def verify_remote(base: str, records: dict[str, tuple[str, int]]) -> None:
    for relative, expected in records.items():
        value = hashlib.sha256()
        length = 0
        url = f"{base}/{urllib.parse.quote(relative, safe='/')}"
        with urllib.request.urlopen(url, timeout=30) as response:
            while chunk := response.read(1024 * 1024):
                value.update(chunk)
                length += len(chunk)
        if (value.hexdigest(), length) != expected:
            raise fail(f"remote proof file differs: {relative}")


def activate(source_value: str, state_value: str) -> None:
    source = directory(source_value, exists=True)
    state_root = private_state(state_value)
    state = read_state(state_root)
    publication = directory(state["publication"], exists=True)
    if state["phase"] != "bootstrap" or any(publication.iterdir()):
        raise fail("proof server is not at bootstrap")
    if not all(owned_process(state, role)[1] for role in ("http", "cloudflared")):
        raise fail("proof server processes are not owned")
    staging = Path(tempfile.mkdtemp(prefix=f".{publication.name}.activate-", dir=publication.parent))
    renamed = False
    try:
        shutil.copytree(source, staging, dirs_exist_ok=True, symlinks=True)
        records = validate_pair(staging)
        make_read_only(staging)
        staging.chmod(0o755)
        publication.chmod(0o755)
        os.replace(staging, publication)
        renamed = True
        publication.chmod(0o555)
        try:
            verify_remote(state["url"], records)
            state["phase"] = "active"
            write_state(state_root, state, replace=True)
        except BaseException:
            temporary_state = state_root / "state.json.new"
            if temporary_state.exists() and not temporary_state.is_symlink():
                temporary_state.unlink()
            publication.chmod(0o755)
            rejected = Path(
                tempfile.mkdtemp(prefix=f".{publication.name}.rejected-", dir=publication.parent)
            )
            rejected.rmdir()
            os.rename(publication, rejected)
            publication.mkdir(mode=0o555)
            publication.chmod(0o555)
            remove_tree(rejected)
            raise
    finally:
        if not renamed and staging.exists():
            remove_tree(staging)


def status(state_value: str) -> int:
    state = read_state(private_state(state_value))
    owned = [owned_process(state, role)[1] for role in ("http", "cloudflared")]
    print(f"url={state['url']}")
    print(f"phase={state['phase']}")
    return 0 if all(owned) else 1


def stop(state_value: str) -> None:
    root = private_state(state_value)
    state = read_state(root)
    processes = [owned_process(state, role) for role in ("cloudflared", "http")]
    def alive(pid: int) -> bool:
        result = subprocess.run(
            ["ps", "-p", str(pid), "-o", "stat="], capture_output=True, text=True
        )
        status = result.stdout.strip()
        return result.returncode == 0 and bool(status) and not status.startswith("Z")

    if any(not owned and alive(pid) for pid, owned in processes):
        raise fail("refusing a foreign process")
    for pid, owned in processes:
        if owned:
            try:
                os.killpg(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and any(alive(pid) for pid, _ in processes):
        time.sleep(0.1)
    for pid, _ in processes:
        if alive(pid):
            try:
                os.killpg(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
    publication = directory(state["publication"], exists=True)
    if state["phase"] == "bootstrap" and any(publication.iterdir()):
        raise fail("bootstrap publication is not empty")
    remove_tree(publication)
    if {path.name for path in root.iterdir()} != set(STATE_FILES):
        raise fail("unexpected proof state file")
    shutil.rmtree(root)


def serve(publication_value: str, port_value: str) -> None:
    publication = directory(publication_value, exists=True)
    if publication.stat().st_mode & 0o222:
        raise fail("served root is writable")
    handler = lambda *args, **kwargs: http.server.SimpleHTTPRequestHandler(
        *args, directory=str(publication), **kwargs
    )
    http.server.ThreadingHTTPServer(("127.0.0.1", int(port_value)), handler).serve_forever()


def main(arguments: list[str]) -> int:
    commands = {
        "bootstrap": (3, bootstrap), "activate": (2, activate), "status": (1, status),
        "stop": (1, stop), "_serve": (2, serve),
    }
    if not arguments or arguments[0] not in commands or len(arguments[1:]) != commands[arguments[0]][0]:
        raise fail("usage: bootstrap SERVED STATE PORT | activate PAIR STATE | status STATE | stop STATE")
    result = commands[arguments[0]][1](*arguments[1:])
    return result if isinstance(result, int) else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except (OSError, ValueError, json.JSONDecodeError, urllib.error.URLError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2)
