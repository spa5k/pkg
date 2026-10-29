#!/usr/bin/env python3
"""Stress the pkg self-update on a throwaway Linux machine.

`build` compiles two clients from this checkout. They trust only a new test
release key, and their versions differ. `test` must run as root. It points
github.com at a local HTTPS server, which serves signed test releases. Then it
checks that the installed client always starts after these events:

- SIGKILL, SIGINT, or SIGTERM at a random time during `pkg update`
- one failed system call, injected with strace at each call position
- a full disk in the install, completion, or temporary folder
- parallel updates
- hostile release servers and broken new clients
- slow, late, and failing responses from the release server
- a power cut, which loses every write that was not synced

`test` changes /etc/hosts, the system CA store, and mounts. Run it only on a
throwaway machine, for example an E2B sandbox:

    python3 tools/stress/update.py build
    sudo PKG_STRESS_THROWAWAY=1 python3 tools/stress/update.py test

The `power` part is not in the default parts. It needs FUSE and LazyFS
(https://github.com/dsrhaslab/lazyfs). Set PKG_STRESS_LAZYFS to the lazyfs
binary when it is not on PATH.
"""

import argparse
import collections
import contextlib
import functools
import hashlib
import http.server
import io
import json
import math
import os
import random
import re
import resource
import shutil
import signal
import socket
import ssl
import struct
import subprocess
import sys
import tarfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORK = Path("/var/tmp/pkg-stress")
OLD, NEW = "0.1.0-stress.1", "0.1.0-stress.2"
SYSTEM = "x86_64-linux"
USER = "pkgstress"
HOME = Path("/home") / USER
BIN_DIR = HOME / ".local/bin"
BINARY = BIN_DIR / "pkg"
COMPLETIONS = HOME / ".local/share/pkg/completions"
DOWNLOADS = HOME / ".local/share/pkg/update"
COMPLETION_NAMES = {"pkg.bash", "_pkg", "pkg.fish"}
SUMS = "/spa5k/pkg/releases/latest/download/SHA256SUMS"
HOSTS = ("github.com", "channels.nixos.org")
HOSTS_MARK = "# pkg-stress"
LAZYFS = os.environ.get("PKG_STRESS_LAZYFS", "lazyfs")
ENV = {
    "HOME": str(HOME),
    "PATH": f"{BIN_DIR}:{WORK}/stub:/usr/bin:/bin",
    "USER": USER,
    "LANG": "C.UTF-8",
    "NO_COLOR": "1",
}


def sh(*args, **kwargs):
    kwargs.setdefault("check", True)
    return subprocess.run([str(arg) for arg in args], **kwargs)


# ---------------------------------------------------------------- build


def build():
    """Build the old and the new client with a test release key."""
    WORK.mkdir(parents=True, exist_ok=True)
    key = WORK / "key"
    if not key.exists():
        sh("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "pkg-stress", "-f", key)
    blob = (WORK / "key.pub").read_text().split()[1]
    source = WORK / "src"
    shutil.rmtree(source, ignore_errors=True)
    shutil.copytree(ROOT, source, ignore=shutil.ignore_patterns("target", ".git"))
    client = source / "crates/pkg-cli/src/client.rs"
    text, count = re.subn(
        r"const RELEASE_KEYS: &\[&str\] = &\[.*?\];",
        f'const RELEASE_KEYS: &[&str] = &["{blob}"];',
        client.read_text(),
        flags=re.DOTALL,
    )
    assert count == 1, "RELEASE_KEYS not found"
    client.write_text(text)
    manifest = source / "Cargo.toml"
    original = manifest.read_text()
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    for version, name in ((OLD, "pkg-old"), (NEW, "pkg-new")):
        text, count = re.subn(r'(?m)^version = ".*"$', f'version = "{version}"', original, count=1)
        assert count == 1, "workspace version not found"
        manifest.write_text(text)
        sh("cargo", "build", "--release", "--offline", "-p", "pkg-cli",
           cwd=source, env={**os.environ, "CARGO_TARGET_DIR": str(target)})
        shutil.copy2(target / "release/pkg", WORK / name)
        print(f"built {name}: {sh(WORK / name, '--version', capture_output=True, text=True).stdout.strip()}")


# ---------------------------------------------------------------- server


class Zeros(io.RawIOBase):
    """An endless stream of zero bytes."""

    def readinto(self, buffer):
        buffer[:] = bytes(len(buffer))
        return len(buffer)

    def readable(self):
        return True


def tarball(entries):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz", compresslevel=1) as archive:
        for name, body in entries:
            info = tarfile.TarInfo(name)
            info.mode = 0o755
            if isinstance(body, int):
                info.size = body
                archive.addfile(info, Zeros())
            else:
                info.size = len(body)
                archive.addfile(info, io.BytesIO(body))
    return data.getvalue()


def sign(message):
    path = WORK / "sign/SHA256SUMS"
    path.parent.mkdir(exist_ok=True)
    path.write_bytes(message)
    Path(f"{path}.sig").unlink(missing_ok=True)
    sh("ssh-keygen", "-q", "-Y", "sign", "-f", WORK / "key", "-n", "pkg-release", path,
       capture_output=True)
    return Path(f"{path}.sig").read_bytes()


def release(version, client=None, *, archive=None, sums=None, signature=None):
    """The routes of one signed test release."""
    name = f"pkg-{version}-{SYSTEM}.tar.gz"
    if archive is None:
        archive = tarball([(f"pkg-{version}-{SYSTEM}/bin/pkg", client)])
    if sums is None:
        sums = f"{hashlib.sha256(archive).hexdigest()}  {name}\n".encode()
    if signature is None:
        signature = sign(sums)
    base = f"/spa5k/pkg/releases/download/v{version}"
    return {
        SUMS: ("ok", sums),
        f"{base}/SHA256SUMS.sig": ("ok", signature),
        f"{base}/{name}": ("ok", archive),
    }


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    routes = {}
    sent = collections.Counter()
    stop = threading.Event()

    def log_message(self, *args):
        pass

    def do_GET(self):
        route = self.routes.get(self.path)
        if route is None:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        mode, body = route
        try:
            getattr(self, f"serve_{mode}")(body)
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError, OSError):
            self.close_connection = True

    def head(self, status=200, **headers):
        self.send_response(status)
        for name, value in headers.items():
            self.send_header(name.replace("_", "-"), str(value))
        self.end_headers()

    def serve_ok(self, body):
        self.head(Content_Length=len(body))
        self.wfile.write(body)

    def serve_redirect(self, location):
        self.head(302, Location=location, Content_Length=0)

    def serve_chunked(self, size):
        """Send `size` bytes without a length, in chunks."""
        self.head(Transfer_Encoding="chunked")
        chunk = bytes(1 << 20)
        while self.sent[self.path] < size:
            self.wfile.write(b"%x\r\n%s\r\n" % (len(chunk), chunk))
            self.sent[self.path] += len(chunk)
        self.wfile.write(b"0\r\n\r\n")

    def serve_length(self, size):
        """Announce `size` bytes, then send them."""
        self.head(Content_Length=size)
        chunk = bytes(1 << 20)
        while self.sent[self.path] < size:
            self.wfile.write(chunk)
            self.sent[self.path] += len(chunk)

    def serve_stall(self, _):
        self.head(Content_Length=100)
        self.stop.wait(600)

    def serve_late(self, body_and_delay):
        body, delay = body_and_delay
        self.stop.wait(delay)
        self.serve_ok(body)

    def serve_throttle(self, body_and_rate):
        """Send the body at `rate` bytes per second."""
        body, rate = body_and_rate
        self.head(Content_Length=len(body))
        step = max(1, rate // 10)
        for start in range(0, len(body), step):
            self.wfile.write(body[start : start + step])
            time.sleep(0.1)

    def serve_truncate(self, body):
        """Announce the body, send half, and close the connection normally."""
        self.head(Content_Length=len(body))
        self.wfile.write(body[: len(body) // 2])
        self.close_connection = True

    def serve_status(self, status):
        self.head(status, Content_Length=0)

    def serve_reset(self, body):
        self.head(Content_Length=len(body))
        self.wfile.write(body[: len(body) // 2])
        self.wfile.flush()
        self.connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        self.close_connection = True
        self.connection.close()


def serve():
    tls = WORK / "tls"
    tls.mkdir(exist_ok=True)
    if not (tls / "server.pem").exists():
        sh("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
           "-nodes", "-days", "2", "-subj", "/CN=pkg-stress CA", "-keyout", tls / "ca.key",
           "-out", tls / "ca.pem", capture_output=True)
        sh("openssl", "req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
           "-subj", "/CN=github.com", "-keyout", tls / "server.key", "-out", tls / "server.csr",
           capture_output=True)
        (tls / "ext").write_text("subjectAltName=" + ",".join(f"DNS:{h}" for h in HOSTS) + "\n")
        sh("openssl", "x509", "-req", "-in", tls / "server.csr", "-CA", tls / "ca.pem",
           "-CAkey", tls / "ca.key", "-CAcreateserial", "-days", "2", "-extfile", tls / "ext",
           "-out", tls / "server.pem", capture_output=True)
    shutil.copy(tls / "ca.pem", "/usr/local/share/ca-certificates/pkg-stress.crt")
    sh("update-ca-certificates", capture_output=True)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(tls / "server.pem", tls / "server.key")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 443), Handler)
    server.daemon_threads = True
    # A stopped client breaks its connection. Do not print each traceback.
    server.handle_error = lambda *_: None
    server.socket = context.wrap_socket(server.socket, server_side=True)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


@contextlib.contextmanager
def machine():
    """Point the hosts at the local server and add the test user."""
    hosts = Path("/etc/hosts")
    original = hosts.read_text()
    hosts.write_text(original + "".join(f"127.0.0.1 {h} {HOSTS_MARK}\n" for h in HOSTS))
    if sh("id", USER, capture_output=True, check=False).returncode:
        sh("useradd", "-m", "-s", "/bin/sh", USER)
    stub = WORK / "stub/nix"
    stub.parent.mkdir(exist_ok=True)
    stub.write_text("#!/bin/sh\necho 'error: the stress test has no Nix' >&2\nexit 1\n")
    stub.chmod(0o755)
    os.chmod(WORK, 0o755)
    server = serve()
    try:
        yield
    finally:
        Handler.stop.set()
        server.shutdown()
        hosts.write_text(original)
        Path("/usr/local/share/ca-certificates/pkg-stress.crt").unlink(missing_ok=True)
        sh("update-ca-certificates", "--fresh", capture_output=True)


# ---------------------------------------------------------------- trials


def chown(path):
    for parent, folders, files in os.walk(path):
        for name in [parent, *(os.path.join(parent, n) for n in folders + files)]:
            shutil.chown(name, USER, USER)


def reset():
    sh("pkill", "-9", "-u", USER, check=False)
    time.sleep(0.05)
    for path in (HOME / ".local", HOME / ".cache"):
        shutil.rmtree(path, ignore_errors=True)
    for path in Path("/tmp").glob("pkg-update-*"):
        shutil.rmtree(path, ignore_errors=True)
    BIN_DIR.mkdir(parents=True)
    shutil.copy2(WORK / "pkg-old", BINARY)
    chown(HOME / ".local")


def start(args, *, strace=None, env=None):
    command = [str(BINARY), *args]
    kwargs = {"user": USER, "group": USER, "extra_groups": []}
    if strace:
        output = [] if "-o" in strace else ["-o", "/dev/null"]
        command = ["strace", "-f", "-qq", *output, *strace, "-u", USER, "--", *command]
        kwargs = {}
    return subprocess.Popen(command, env={**ENV, **(env or {})}, cwd=HOME, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            start_new_session=True, **kwargs)


def finish(process, timeout):
    try:
        out, err = process.communicate(timeout=timeout)
        return process.returncode, out, err
    except subprocess.TimeoutExpired:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        out, err = process.communicate()
        return "hang", out, err


def inspect(result):
    """Classify the machine after one trial."""
    code, out, err = result
    orphans = sh("pgrep", "-u", USER, "-a", capture_output=True, text=True, check=False).stdout
    version = sh(BINARY, "--version", user=USER, env=ENV, capture_output=True, text=True,
                 check=False, timeout=30) if BINARY.exists() else None
    reported = version.stdout.strip() if version and version.returncode == 0 else None
    state = {f"pkg {OLD}": "old", f"pkg {NEW}": "new"}.get(reported, "broken")
    leftovers = sorted(
        [f"bin/{p.name[:9]}*" for p in BIN_DIR.iterdir() if p.name != "pkg"]
        + [f"completions/{p.name[:4]}*" for p in COMPLETIONS.glob("*")
           if p.name not in COMPLETION_NAMES]
        + [f"tmp/{p.name[:11]}*" for p in Path("/tmp").glob("pkg-update-*")]
        + [f"downloads/{p.name[:11]}*" for p in DOWNLOADS.glob("*")]
        + [f"process:{line.split()[1].rsplit('/', 1)[-1]}" for line in orphans.splitlines()]
    ) if BIN_DIR.exists() else ["bin folder missing"]
    lines = [line for line in err.splitlines() if line.strip()]
    message = next((line for line in lines if line.startswith(("Could not", "error"))),
                   lines[-1] if lines else "")
    # A completion file must not be empty. A power cut empties a file whose
    # data was not synced before the rename.
    damaged = sorted(f"completions/{p.name}" for p in COMPLETIONS.glob("*")
                     if p.name in COMPLETION_NAMES and p.stat().st_size == 0)
    return {
        "code": code,
        "state": state,
        "leftovers": sorted(set(leftovers)),
        "damaged": damaged,
        "panic": "panicked" in err or "RUST_BACKTRACE" in err,
        "escape": "\x1b" in out + err,
        "updated": "Updated pkg" in out,
        "message": re.sub(r"pkg-update-\w+|\.pkg-new-\w+|\.tmp\w+", "…", message)[:160],
        "stderr": err[-600:],
    }


def bad(trial):
    """Whether one trial broke an invariant."""
    expect = trial.get("expect")
    return bool(trial["state"] == "broken" or trial["panic"] or trial.get("damaged")
                or (trial["updated"] and trial["state"] != "new")
                or (expect and (trial["state"] != expect or trial["leftovers"])))


def update(**kwargs):
    reset()
    timeout = kwargs.pop("timeout", 120)
    return inspect(finish(start(["update"], **kwargs), timeout))


# ---------------------------------------------------------------- scenarios


@functools.cache
def normal_routes():
    return release(NEW, (WORK / "pkg-new").read_bytes())


def normal_release():
    return dict(normal_routes())


def baseline(report):
    Handler.routes = normal_release()
    timings = []
    for _ in range(3):
        began = time.monotonic()
        trial = update()
        timings.append(time.monotonic() - began)
        report("baseline", trial)
    assert trial["state"] == "new" and trial["updated"], trial
    assert not trial["leftovers"], trial
    return sorted(timings)[1]


def kills(report, duration, rounds):
    Handler.routes = normal_release()
    for name, number, share in (("SIGKILL", signal.SIGKILL, 1), ("SIGINT", signal.SIGINT, 0.5),
                                ("SIGTERM", signal.SIGTERM, 0.3)):
        for _ in range(max(1, int(rounds * share))):
            reset()
            process = start(["update"])
            delay = random.uniform(0, duration * 1.1)
            time.sleep(delay)
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, number)
            trial = inspect(finish(process, 60))
            trial["delay"] = round(delay, 3)
            report(f"kill {name}", trial)
            # The next update must finish the job and remove every leftover.
            after = inspect(finish(start(["update"]), 120))
            after["expect"] = "new"
            report(f"kill {name}: next update", after)


INJECT = [
    ("openat", "EACCES"), ("openat", "EMFILE"), ("read", "EIO"), ("read", "EINTR"),
    ("write", "ENOSPC"), ("write", "EINTR"), ("fsync", "EIO"), ("rename", "EIO"),
    ("renameat", "EIO"), ("renameat2", "EIO"), ("execve", "ENOEXEC"), ("clone3", "EAGAIN"),
    ("clone", "EAGAIN"), ("fchmod", "EPERM"), ("mkdir", "EACCES"), ("unlink", "EBUSY"),
    ("unlinkat", "EBUSY"), ("rmdir", "EBUSY"), ("wait4", "EINTR"), ("poll", "EINTR"),
    ("ppoll", "EINTR"), ("pipe2", "EMFILE"), ("readlink", "EIO"), ("getrandom", "EAGAIN"),
]


def positions(count, limit):
    if count <= limit:
        return list(range(1, count + 1))
    head = list(range(1, limit // 2 + 1))
    steps = max(2, limit - len(head))
    tail = {round(math.exp(math.log(len(head) + 1)
                           + i * (math.log(count) - math.log(len(head) + 1)) / (steps - 1)))
            for i in range(steps)}
    return sorted(set(head) | tail)


def faults(report, limit, calls):
    Handler.routes = normal_release()
    reset()
    trace = WORK / "trace"
    inject = [(name, error) for name, error in INJECT if not calls or name in calls]
    names = sorted({name for name, _ in inject})
    process = start(["update"], strace=["-o", str(trace), "-e", f"trace={','.join(names)}"])
    finish(process, 120)
    counts = collections.Counter()
    for line in trace.read_text().splitlines():
        match = re.match(r"(\d+)\s+(\w+)\(", line)
        if match:
            counts[match.groups()] += 1
    most = collections.Counter()
    for (_, name), number in counts.items():
        most[name] = max(most[name], number)
    print("calls per process (max):", dict(sorted(most.items())), flush=True)
    for name, error in inject:
        for when in positions(most[name], limit):
            trial = update(strace=["-e", f"trace={name}",
                                   "-e", f"inject={name}:error={error}:when={when}"])
            trial["fault"] = f"{name}#{when}={error}"
            report(f"fault {name}={error}", trial)


@contextlib.contextmanager
def small_mount(path, size):
    path.mkdir(parents=True, exist_ok=True)
    sh("mount", "-t", "tmpfs", "-o", f"size={size},mode=0755", "tmpfs", path)
    try:
        yield
    finally:
        sh("umount", "-l", path, check=False)


def full_disks(report):
    Handler.routes = normal_release()
    old = (WORK / "pkg-old").stat().st_size
    reset()
    with small_mount(BIN_DIR, old + (1 << 20)):
        shutil.copy2(WORK / "pkg-old", BINARY)
        chown(BIN_DIR)
        trial = inspect(finish(start(["update"]), 120))
        report("full disk: bin", trial)
    reset()
    with small_mount(COMPLETIONS, 4096):
        chown(HOME / ".local")
        trial = inspect(finish(start(["update"]), 120))
        report("full disk: completions", trial)
    reset()
    folder = Path("/mnt/pkg-stress-tmp")
    with small_mount(folder, 1 << 20):
        os.chmod(folder, 0o1777)
        trial = inspect(finish(start(["update"], env={"TMPDIR": str(folder)}), 120))
        trial["leftovers"] += [f"TMPDIR/{p.name[:11]}*" for p in folder.iterdir()]
        report("full disk: TMPDIR", trial)


def parallel(report, rounds):
    Handler.routes = normal_release()
    for _ in range(rounds):
        reset()
        processes = [start(["update"]) for _ in range(6)]
        results = [finish(process, 180) for process in processes]
        trial = inspect(results[0])
        trial["updated"] = sum("Updated pkg" in out for _, out, _ in results)
        trial["panic"] = any("panicked" in err for _, _, err in results)
        trial["code"] = sorted(str(code) for code, _, _ in results)
        report("parallel x6", trial)


def script(body):
    return f"#!/bin/sh\n{body}\n".encode()


def hostile(report):
    new = (WORK / "pkg-new").read_bytes()
    normal = normal_release()
    name = f"pkg-{NEW}-{SYSTEM}.tar.gz"
    base = f"/spa5k/pkg/releases/download/v{NEW}"
    cases = {
        "SHA256SUMS: 400 MiB, no length": {**normal, SUMS: ("chunked", 400 << 20)},
        "SHA256SUMS: 400 MiB length": {**normal, SUMS: ("length", 400 << 20)},
        "SHA256SUMS: server stalls": {**normal, SUMS: ("stall", None)},
        "SHA256SUMS: redirect to http": {**normal, SUMS: ("redirect", f"http://github.com{SUMS}")},
        "archive: connection reset": {**normal, f"{base}/{name}": ("reset", normal[f"{base}/{name}"][1])},
        "archive: 400 MiB, no length": {**normal, f"{base}/{name}": ("chunked", 400 << 20)},
        "archive: checksum mismatch": {**normal, f"{base}/{name}": ("ok", tarball([(f"pkg-{NEW}-{SYSTEM}/bin/pkg", new[:-1])]))},
        "signature: missing": {k: v for k, v in normal.items() if not k.endswith(".sig")},
        "signature: other message": {**normal, f"{base}/SHA256SUMS.sig": ("ok", sign(b"other\n"))},
        "release: older version": release("0.0.1", script("exit 1")),
        "release: 300 MiB client": release(NEW, archive=tarball([(f"pkg-{NEW}-{SYSTEM}/bin/pkg", 300 << 20)])),
        "release: symlink member": release(NEW, archive=symlink_archive()),
        "client: wrong version": release(NEW, script("printf 'pkg 9.9.9\\033[2J\\n'")),
        "client: hangs": release(NEW, script("exec sleep 600")),
        "client: crashes": release(NEW, script("kill -SEGV $$")),
        "client: fish completion fails": release(NEW, script(
            f'case "$1 $2" in "--version ") echo "pkg {NEW}";; "completion fish") exit 3;;'
            ' *) echo "# $2";; esac')),
    }
    for label, routes in cases.items():
        Handler.routes = routes
        Handler.sent.clear()
        began = time.monotonic()
        before = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
        trial = update(timeout=100)
        after = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
        trial["seconds"] = round(time.monotonic() - began, 1)
        trial["sent_mib"] = {path.rsplit("/", 1)[-1]: n >> 20 for path, n in Handler.sent.items()}
        # Linux keeps one maximum over all children, so only a new maximum is exact.
        trial["max_rss_mib"] = after >> 10 if after > before else None
        report(f"hostile: {label}", trial)


def network(report):
    """Slow, late, and failing responses. A slow but working server must not
    stop the update. A failure must leave the old client and no files."""
    normal = normal_release()
    path = next(path for path in normal if path.endswith(".tar.gz"))
    archive = normal[path][1]
    cases = {
        "SHA256SUMS: 45 s before the response": ({**normal, SUMS: ("late", (normal[SUMS][1], 45))}, "new"),
        "archive: 64 KiB/s": ({**normal, path: ("throttle", (archive, 64 << 10))}, "new"),
        "archive: 400 s at a constant rate": ({**normal, path: ("throttle", (archive, len(archive) // 400))}, "old"),
        "archive: connection closes early": ({**normal, path: ("truncate", archive)}, "old"),
        "SHA256SUMS: HTTP 429": ({**normal, SUMS: ("status", 429)}, "old"),
        "archive: HTTP 503": ({**normal, path: ("status", 503)}, "old"),
    }
    for label, (routes, expect) in cases.items():
        Handler.routes = routes
        began = time.monotonic()
        trial = update(timeout=420)
        trial["seconds"] = round(time.monotonic() - began, 1)
        trial["archive_kib"] = len(archive) >> 10
        trial["expect"] = expect
        report(f"network: {label}", trial)


@contextlib.contextmanager
def lazyfs(path):
    """Mount LazyFS on `path`. LazyFS keeps written data in memory until an
    fsync. The yielded function drops that data, like a power cut. Renames,
    new files, and other metadata changes stay."""
    root, fifo, log = WORK / "lazyfs-root", WORK / "lazyfs.fifo", WORK / "lazyfs.log"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    log.unlink(missing_ok=True)
    path.mkdir(parents=True, exist_ok=True)
    config = WORK / "lazyfs.toml"
    config.write_text(f'[faults]\nfifo_path="{fifo}"\n[cache]\napply_eviction=false\n'
                      '[cache.simple]\ncustom_size="1gb"\nblocks_per_page=1\n'
                      f'[filesystem]\nlog_all_operations=false\nlogfile="{log}"\n')
    process = subprocess.Popen([LAZYFS, path, "--config-path", config, "-o", "allow_other",
                                "-o", "modules=subdir", "-o", f"subdir={root}", "-f"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.monotonic() + 10
    while not os.path.ismount(path):
        assert process.poll() is None and time.monotonic() < deadline, "LazyFS did not mount"
        time.sleep(0.1)

    def cut():
        # LazyFS logs one line when the cache is clear.
        done = log.read_text().count("cache is cleared") + 1
        fifo.write_text("lazyfs::clear-cache\n")
        while log.read_text().count("cache is cleared") < done:
            time.sleep(0.05)

    try:
        yield cut
    finally:
        sh("umount", "-l", path, check=False)
        process.kill()
        process.wait()


def power(report, duration, rounds):
    """Cut the power during an update, or just after it. Then the next
    update must finish the job."""
    Handler.routes = normal_release()
    with lazyfs(HOME / ".local") as cut:
        for index in range(rounds):
            reset()
            fd = os.open(BINARY, os.O_RDONLY)
            os.fsync(fd)
            os.close(fd)
            process = start(["update"])
            label, delay = "power cut after update", None
            if index % 3:
                label, delay = "power cut", random.uniform(0, duration * 1.1)
                time.sleep(delay)
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(process.pid, signal.SIGKILL)
            result = finish(process, 120)
            cut()
            trial = inspect(result)
            trial["delay"] = delay and round(delay, 3)
            report(label, trial)
            after = inspect(finish(start(["update"]), 120))
            after["expect"] = "new"
            report(f"{label}: next update", after)


def symlink_archive():
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as archive:
        info = tarfile.TarInfo(f"pkg-{NEW}-{SYSTEM}/bin/pkg")
        info.type = tarfile.SYMTYPE
        info.linkname = "/bin/sh"
        archive.addfile(info)
    return data.getvalue()


# ---------------------------------------------------------------- main


def test(arguments):
    if os.geteuid() != 0 or os.environ.get("PKG_STRESS_THROWAWAY") != "1":
        sys.exit("Run `test` as root with PKG_STRESS_THROWAWAY=1 on a throwaway machine.")
    if os.environ.get("PKG_STRESS_NETNS") != "1":
        # Run in a network namespace with only loopback. Then a failed hosts
        # lookup cannot reach the real GitHub.
        os.environ["PKG_STRESS_NETNS"] = "1"
        script = 'ip link set lo up && exec "$0" "$@"'
        os.execvp("unshare", ["unshare", "--net", "--", "sh", "-c", script,
                              sys.executable, *sys.argv])
    random.seed(arguments.seed)
    results = []
    log = (WORK / "results.jsonl").open("w")

    def report(scenario, trial):
        trial["scenario"] = scenario
        results.append(trial)
        log.write(json.dumps(trial) + "\n")
        log.flush()
        flag = "BAD " if bad(trial) else ""
        print(f"{flag}{scenario}: {trial['state']} code={trial['code']} "
              f"leftovers={trial['leftovers']} {trial.get('fault', '')} {trial['message']}",
              flush=True)

    with machine():
        duration = baseline(report)
        print(f"update takes {duration:.2f} s", flush=True)
        for part in arguments.parts:
            if part == "kill":
                kills(report, duration, arguments.rounds)
            elif part == "fault":
                faults(report, arguments.positions, arguments.calls)
            elif part == "disk":
                full_disks(report)
            elif part == "parallel":
                parallel(report, 5)
            elif part == "hostile":
                hostile(report)
            elif part == "network":
                network(report)
            elif part == "power":
                power(report, duration, arguments.rounds)
    summary(results)
    sys.exit(1 if any(bad(trial) for trial in results) else 0)


def summary(results):
    groups = collections.Counter()
    for trial in results:
        group = trial["scenario"].split(":")[0] if trial["scenario"].startswith("fault") else trial["scenario"]
        groups[(group, trial["state"], tuple(trial["leftovers"]), bad(trial))] += 1
    print("\nscenario | state | leftovers | trials")
    for (scenario, state, leftovers, broken), number in sorted(groups.items()):
        print(f"{'BAD ' if broken else ''}{scenario} | {state} | {', '.join(leftovers) or '-'} | {number}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("build")
    run = commands.add_parser("test")
    run.add_argument("parts", nargs="*",
                     default=["kill", "fault", "disk", "parallel", "hostile", "network"])
    run.add_argument("--rounds", type=int, default=300, help="SIGKILL and power cut trials")
    run.add_argument("--positions", type=int, default=24, help="fault positions per call")
    run.add_argument("--calls", nargs="*", default=[], help="inject faults only into these calls")
    run.add_argument("--seed", type=int, default=1)
    arguments = parser.parse_args()
    build() if arguments.command == "build" else test(arguments)


if __name__ == "__main__":
    main()
