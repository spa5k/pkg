#!/usr/bin/env python3
"""Serve one sealed DN-1 proof pair over loopback TLS inside the proof VM.

The proof product runs inside the tart VM, so the baked channel URL
(https://127.0.0.1:8443) is only correct when the channel server runs in
the same VM. This tool wraps an already-digest-verified pair directory in
a disposable loopback TLS endpoint:

- validate the pair exactly as the sealed inventories describe it;
- move it to its final read-only publication path with one rename;
- generate a disposable CA and one server certificate (SAN 127.0.0.1);
- serve n/ and n-plus-1/ on 127.0.0.1:<port> over TLS only;
- verify every served file through the live TLS endpoint before activation;
- stop cleanly and remove every byte it created.

The CA is not trusted by this tool. The workflow installs it into the VM
System keychain before the product runs and removes it in teardown.

Usage:
  bootstrap STAGING PUBLICATION STATE PORT
  status STATE
  stop STATE
  _serve PUBLICATION PORT STATE
"""

import hashlib
import http.server
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import socket
import ssl
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


PAIR_FILES = {"n", "n-plus-1", "n.inventory.json", "n-plus-1.inventory.json", "proof-pair.json"}
CERT_FILES = {
    "ca.cnf", "server.cnf", "ca.pem", "ca-key.pem", "ca.srl",
    "server.csr", "server.pem", "server-key.pem",
}
STATE_FILES = {*CERT_FILES, "state.json", "http.log"}
CA_COMMON_NAME = "pkg-dn1-loopback-ca"
SERVER_DNS_NAME = "localhost"
LOOPBACK_IP = "127.0.0.1"
URL = re.compile(r"https://127\.0\.0\.1:[0-9]{4,5}")


def fail(message: str) -> ValueError:
    return ValueError(message)


def directory(value: str, *, exists: bool) -> Path:
    supplied = Path(value)
    if not supplied.is_absolute() or supplied.is_symlink():
        raise fail("directory must be an absolute non-symlink path")
    return supplied.resolve(strict=exists)


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
            length = length + len(chunk)
    return value.hexdigest(), length


def safe_relative(value: object) -> str:
    if not isinstance(value, str):
        raise fail("inventory path is not text")
    path = PurePosixPath(value)
    if path.is_absolute() or not path.parts or any(part in ("", ".", "..") for part in path.parts):
        raise fail("inventory path is unsafe")
    return value


def validate_pair(root: Path) -> dict[str, tuple[str, int]]:
    """Return the exact served-path map for a sealed pair directory."""
    metadata = root.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or root.is_symlink() or {path.name for path in root.iterdir()} != PAIR_FILES:
        raise fail("proof pair has unexpected top-level entries")
    if any(path.is_symlink() or not (path.is_dir() or path.is_file()) for path in root.rglob("*")):
        raise fail("proof pair contains a symlink or special file")
    descriptor, descriptor_raw = load_json(root / "proof-pair.json", 64 * 1024)
    if (
        set(descriptor) != {"schemaVersion", "channels", "productCommit"}
        or descriptor["schemaVersion"] != 1
        or not isinstance(descriptor["productCommit"], str)
        or re.fullmatch(r"[0-9a-f]{40}", descriptor["productCommit"]) is None
    ):
        raise fail("invalid proof pair descriptor")
    channels = descriptor["channels"]
    if not isinstance(channels, list) or len(channels) != 2:
        raise fail("proof pair must contain two channels")

    records = {"proof-pair.json": (hashlib.sha256(descriptor_raw).hexdigest(), len(descriptor_raw))}
    trusted_root = None
    channels = list(channels)
    if len(channels) != 2:
        raise fail("proof pair must bind exactly two channels")
    for channel, name, version in zip(channels, ("n", "n-plus-1"), (1, 2)):
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


CA_CONFIG = """[req]
distinguished_name = ca_dn
prompt = no

[ca_dn]
CN = {common_name}

[v3_ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
"""

SERVER_CONFIG = """[req]
distinguished_name = server_dn
prompt = no

[server_dn]
CN = {ip}

[v3_server]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = IP:{ip},DNS:{dns}
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
"""


def run_openssl(arguments: list[str], state: Path) -> None:
    result = subprocess.run(
        ["/usr/bin/env", "openssl", *arguments],
        capture_output=True, text=True, cwd=state,
    )
    if result.returncode != 0:
        raise fail(f"openssl failed: {result.stderr.strip()[:512]}")


def generate_certificate_material(state: Path) -> None:
    (state / "ca.cnf").write_text(CA_CONFIG.format(common_name=CA_COMMON_NAME))
    (state / "server.cnf").write_text(SERVER_CONFIG.format(ip=LOOPBACK_IP, dns=SERVER_DNS_NAME))
    for name in ("ca.cnf", "server.cnf"):
        (state / name).chmod(0o600)
    run_openssl([
        "req", "-new", "-x509", "-newkey", "rsa:2048", "-sha256",
        "-keyout", "ca-key.pem", "-out", "ca.pem", "-days", "2", "-nodes",
        "-config", "ca.cnf", "-extensions", "v3_ca",
    ], state)
    run_openssl([
        "req", "-new", "-newkey", "rsa:2048", "-sha256",
        "-keyout", "server-key.pem", "-out", "server.csr", "-nodes",
        "-config", "server.cnf",
    ], state)
    run_openssl([
        "x509", "-req", "-sha256", "-days", "2",
        "-in", "server.csr", "-CA", "ca.pem", "-CAkey", "ca-key.pem",
        "-CAcreateserial", "-out", "server.pem",
        "-extfile", "server.cnf", "-extensions", "v3_server",
    ], state)
    for name in ("ca.pem", "ca.srl", "server.csr", "server.pem"):
        (state / name).chmod(0o644)
    for name in ("ca-key.pem", "server-key.pem"):
        (state / name).chmod(0o600)
    if {path.name for path in state.iterdir()} != CERT_FILES:
        raise fail("certificate generation produced unexpected state files")


def client_context(state: Path) -> ssl.SSLContext:
    context = ssl.create_default_context(cafile=str(state / "ca.pem"))
    context.check_hostname = True
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    return context


def read_state(root: Path) -> dict:
    state, _ = load_json(root / "state.json", 4096)
    expected = {
        "version", "phase", "url", "publication", "port", "http_pid",
        "verified_files", "verified_bytes",
    }
    if (
        set(state) != expected or state["version"] != 1 or state["phase"] != "active"
        or not isinstance(state["url"], str) or URL.fullmatch(state["url"]) is None
        or not isinstance(state["publication"], str)
        or not isinstance(state["port"], int) or not 1024 <= state["port"] <= 65535
        or not isinstance(state["http_pid"], int) or state["http_pid"] <= 1
        or not isinstance(state["verified_files"], int) or state["verified_files"] < 0
        or not isinstance(state["verified_bytes"], int) or state["verified_bytes"] < 0
    ):
        raise fail("invalid loopback server state")
    return state


def write_state(root: Path, state: dict) -> None:
    descriptor = os.open(
        root / "state.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
    )
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(state, stream, sort_keys=True, separators=(",", ":"))
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def owned_process(state: dict) -> tuple[int, bool]:
    pid = state["http_pid"]
    markers = (str(Path(__file__).resolve()), "_serve", str(state["publication"]))
    result = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True)
    alive = result.returncode == 0 and bool(result.stdout.strip())
    return pid, alive and all(marker in result.stdout for marker in markers)


def process_alive(pid: int) -> bool:
    result = subprocess.run(["ps", "-p", str(pid), "-o", "stat="], capture_output=True, text=True)
    status = result.stdout.strip()
    return result.returncode == 0 and bool(status) and not status.startswith("Z")


def terminate(pid: int) -> None:
    try:
        os.killpg(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and process_alive(pid):
        time.sleep(0.1)
    if process_alive(pid):
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and process_alive(pid):
            time.sleep(0.1)


def port_open(port: int) -> bool:
    with socket.socket() as probe:
        probe.settimeout(1)
        return probe.connect_ex((LOOPBACK_IP, int(port))) == 0


def tls_fetch(url: str, context: ssl.SSLContext, maximum: int) -> tuple[str, int]:
    value = hashlib.sha256()
    length = 0
    with urllib.request.urlopen(url, timeout=30, context=context) as response:
        if response.status != 200:
            raise fail(f"loopback fetch returned {response.status}")
        while chunk := response.read(1024 * 1024):
            if length + len(chunk) > maximum:
                raise fail("loopback response exceeds its inventory length")
            value.update(chunk)
            length = length + len(chunk)
    return value.hexdigest(), length


def verify_live(port: int, records: dict[str, tuple[str, int]], state: Path) -> tuple[int, int]:
    context = client_context(state)
    base = f"https://{LOOPBACK_IP}:{port}"
    for relative, expected in sorted(records.items()):
        url = f"{base}/{urllib.parse.quote(relative, safe='/')}"
        if tls_fetch(url, context, expected[1]) != expected:
            raise fail(f"loopback endpoint serves different bytes: {relative}")
    return len(records), sum(expected[1] for expected in records.values())


def make_read_only(root: Path) -> None:
    for path in root.rglob("*"):
        path.chmod(0o555 if path.is_dir() else 0o444)
    root.chmod(0o555)


def make_writable(root: Path) -> None:
    for path in root.rglob("*"):
        path.chmod(0o700 if path.is_dir() else 0o600)
    root.chmod(0o700)


def remove_tree(root: Path) -> None:
    for path in root.rglob("*"):
        if path.is_symlink():
            raise fail("refusing to remove a symlinked tree")
    make_writable(root)
    shutil.rmtree(root)


def wait_live(port: int, context: ssl.SSLContext, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    url = f"https://{LOOPBACK_IP}:{port}/"
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2, context=context) as response:
                if response.status == 200:
                    return
        except (OSError, urllib.error.URLError, ssl.SSLError):
            pass
        time.sleep(0.25)
    raise fail("loopback endpoint did not become reachable")


def bootstrap(staging_value: str, publication_value: str, state_value: str, port_value: str) -> None:
    staging = directory(staging_value, exists=True)
    port = int(port_value)
    if not 1024 <= port <= 65535:
        raise fail("port is outside the unprivileged range")
    publication = Path(publication_value)
    if not publication.is_absolute() or publication.is_symlink() or publication.exists():
        raise fail("publication must be an absolute absent path")
    if not publication.parent.is_dir() or publication.parent.is_symlink():
        raise fail("publication parent must be an existing directory")
    publication = publication.parent.resolve(strict=True) / publication.name
    if publication.exists() or publication.parent != staging.parent:
        raise fail("publication must be a sibling of the staging directory")
    state_root = private_state(state_value, create=True)
    renamed = False
    server = None
    try:
        records = validate_pair(staging)
        # macOS refuses to rename a directory without write permission on
        # the directory itself (the rename must update its '..' entry), so
        # the tree is renamed FIRST and made read-only AFTER.
        os.replace(staging, publication)
        renamed = True
        make_read_only(publication)
        generate_certificate_material(state_root)
        log = (state_root / "http.log").open("xb")
        server = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), "_serve",
             str(publication), str(port), str(state_root)],
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        log.close()
        wait_live(port, client_context(state_root), 15)
        verified_files, verified_bytes = verify_live(port, records, state_root)
        url = f"https://{LOOPBACK_IP}:{port}"
        write_state(state_root, {
            "version": 1, "phase": "active", "url": url, "port": port,
            "publication": str(publication), "http_pid": server.pid,
            "verified_files": verified_files, "verified_bytes": verified_bytes,
        })
        print(f"url={url}")
        print(f"verified_files={verified_files}")
        print(f"verified_bytes={verified_bytes}")
    except BaseException:
        if server is not None:
            terminate(server.pid)
        if state_root.exists():
            shutil.rmtree(state_root, ignore_errors=True)
        if renamed and publication.exists():
            make_writable(publication)
            os.replace(publication, staging)
        raise


def serve(publication_value: str, port_value: str, state_value: str) -> None:
    publication = directory(publication_value, exists=True)
    state_root = private_state(state_value)
    if publication.stat().st_mode & 0o222:
        raise fail("served root is writable")
    if not (state_root / "ca.pem").is_file():
        raise fail("server state has no CA")
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.minimum_version = ssl.TLSVersion.TLSv1_2
    tls.load_cert_chain(
        certfile=str(state_root / "server.pem"), keyfile=str(state_root / "server-key.pem")
    )

    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=str(publication), **kwargs)

        def log_message(self, format, *args):  # noqa: A002
            pass

    class Server(http.server.ThreadingHTTPServer):
        daemon_threads = True
        allow_reuse_address = False

        def get_request(self):
            connection, address = super().get_request()
            return tls.wrap_socket(connection, server_side=True), address

    Server((LOOPBACK_IP, int(port_value)), Handler).serve_forever()


def status(state_value: str) -> int:
    state = read_state(private_state(state_value))
    alive = owned_process(state)[1]
    print(f"url={state['url']}")
    print(f"phase={state['phase']}")
    return 0 if alive else 1


def stop(state_value: str) -> None:
    root = private_state(state_value)
    state = read_state(root)
    pid, owned = owned_process(state)
    if not owned and process_alive(pid):
        raise fail("refusing a foreign process")
    terminate(pid)
    if port_open(state["port"]):
        raise fail("loopback port is still accepting connections")
    remove_tree(directory(state["publication"], exists=True))
    if {path.name for path in root.iterdir()} != STATE_FILES:
        raise fail("unexpected loopback state file")
    shutil.rmtree(root)
    print(f"stopped url={state['url']}")


def main(arguments: list[str]) -> int:
    commands = {
        "bootstrap": (4, bootstrap), "status": (1, status), "stop": (1, stop),
        "_serve": (3, serve),
    }
    if not arguments or arguments[0] not in commands or len(arguments[1:]) != commands[arguments[0]][0]:
        raise fail(
            "usage: bootstrap STAGING PUBLICATION STATE PORT | status STATE | stop STATE"
        )
    result = commands[arguments[0]][1](*arguments[1:])
    return result if isinstance(result, int) else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except (OSError, ValueError, json.JSONDecodeError, urllib.error.URLError, ssl.SSLError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2)
