#!/usr/bin/env python3
import io
import json
import os
import selectors
import stat
import subprocess
import sys
from pathlib import Path


def private_file(path):
    path.parent.mkdir(mode=0o700, exist_ok=True)
    metadata = os.lstat(path.parent)
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        raise PermissionError("capture directory is not private")
    return os.fdopen(
        os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600),
        "wb",
        buffering=0,
    )


def verified_read(path, limit, expected_uid=0, expected_gid=0):
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    except OSError as error:
        raise ValueError("capture source cannot be opened safely") from error
    try:
        metadata = os.fstat(descriptor)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != expected_uid
            or metadata.st_gid != expected_gid
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_nlink != 1
            or metadata.st_size > limit
        ):
            raise ValueError("capture source identity is invalid")
        chunks = []
        remaining = metadata.st_size
        while remaining:
            chunk = os.read(descriptor, remaining)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        if remaining or os.read(descriptor, 1):
            raise ValueError("capture source changed during read")
        return data
    finally:
        os.close(descriptor)


def run_bounded(limit, command, stdout, stderr):
    process = subprocess.Popen(
        command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, stdout)
    selector.register(process.stderr, selectors.EVENT_READ, stderr)
    captured = 0
    truncated = False
    while selector.get_map():
        for key, _events in selector.select():
            chunk = os.read(key.fileobj.fileno(), 65536)
            if not chunk:
                selector.unregister(key.fileobj)
                key.fileobj.close()
                continue
            available = max(0, limit - captured)
            key.data.write(chunk[:available])
            captured += min(len(chunk), available)
            truncated = truncated or len(chunk) > available
    return process.wait(), captured, truncated


def capture():
    if len(sys.argv) < 7 or sys.argv[5] != "--":
        raise SystemExit("usage: pkg_bounded_capture.py LIMIT STATUS STDOUT STDERR -- COMMAND...")
    limit = int(sys.argv[1])
    if limit < 0 or limit > 1024 * 1024:
        raise SystemExit("capture limit is invalid")
    status_path, stdout_path, stderr_path = map(Path, sys.argv[2:5])
    with private_file(stdout_path) as stdout, private_file(stderr_path) as stderr:
        exit_status, captured, truncated = run_bounded(limit, sys.argv[6:], stdout, stderr)
        os.fsync(stdout.fileno())
        os.fsync(stderr.fileno())
    with private_file(status_path) as status:
        status.write(
            f"exit_status={exit_status}\ncaptured_bytes={captured}\ntruncated={str(truncated).lower()}\n".encode()
        )
        os.fsync(status.fileno())
    return exit_status


def gc_failure_line(output, status, truncated):
    fallback = f"stage=gc exit_status={status} public_error=unavailable"
    if truncated:
        return fallback
    try:
        result = json.loads(output)
    except (UnicodeDecodeError, ValueError, RecursionError):
        return fallback
    allowed = {
        72: "STATE_LOCKED",
        73: "STATE_CORRUPT",
        79: "ENGINE_UNAVAILABLE",
    }
    if not isinstance(result, dict):
        return fallback
    error = result.get("error")
    if not isinstance(error, dict):
        return fallback
    code = error.get("code")
    symbol = error.get("symbol")
    if (
        type(result.get("schemaVersion")) is not int
        or result["schemaVersion"] != 1
        or result.get("ok") is not False
        or result.get("command") != "gc"
        or type(code) is not int
        or code != status
        or allowed.get(code) != symbol
    ):
        return fallback
    return f"stage=gc exit_status={status} symbol={allowed[code]} code={code}"


def capture_gc():
    if len(sys.argv) < 5 or sys.argv[3] != "--":
        raise SystemExit("usage: pkg_bounded_capture.py gc LIMIT -- COMMAND...")
    limit = int(sys.argv[2])
    if limit < 0 or limit > 1024 * 1024:
        raise SystemExit("capture limit is invalid")
    stdout = io.BytesIO()
    stderr = io.BytesIO()
    returncode, _captured, truncated = run_bounded(limit, sys.argv[4:], stdout, stderr)
    status = 128 - returncode if returncode < 0 else returncode
    if status == 0:
        sys.stdout.buffer.write(stdout.getvalue())
        sys.stderr.buffer.write(stderr.getvalue())
    else:
        print(gc_failure_line(stdout.getvalue(), status, truncated))
    return status


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "copy":
        try:
            data = verified_read(Path(sys.argv[2]), int(sys.argv[3]))
        except (OSError, ValueError):
            raise SystemExit(1)
        sys.stdout.buffer.write(data)
        return 0
    if len(sys.argv) >= 2 and sys.argv[1] == "gc":
        return capture_gc()
    return capture()


if __name__ == "__main__":
    raise SystemExit(main())
