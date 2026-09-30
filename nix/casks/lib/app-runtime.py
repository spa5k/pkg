"""Restore external vendor signatures on a private copy, never in the store.

The same entry point serves pkg app sync and generated bundled-command
wrappers. The manifest and payload are immutable Nix data. The cache is
disposable; it is not package inventory. No vendor executable runs during
preparation, and no signature is created or replaced.
"""

import contextlib
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import signal
import stat
import subprocess
import sys
import tempfile
import shutil

SCHEMA = "pkg-cask-apps/1"
MANIFEST = "share/pkg/cask-apps.json"
SOURCE_DIR = "libexec/pkg/app-sources"
ATTRS = frozenset(
    "com.apple.cs." + name
    for name in (
        "CodeDirectory", "CodeEntitlements", "CodeRequirements",
        "CodeRequirements-1", "CodeSignature",
    )
)


def _xattr_call(name, path, *args):
    # Darwin's Python os module has no xattr functions. Use the native
    # calls with XATTR_NOFOLLOW=1; never follow a signature target link.
    library = ctypes.CDLL(None, use_errno=True)
    function = getattr(library, name)
    function.restype = ctypes.c_ssize_t if name != "setxattr" else ctypes.c_int
    function.argtypes = (
        [ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
        if name == "listxattr" else
        [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p,
         ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    )
    result = function(os.fsencode(path), *args)
    if result < 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code), str(path))
    return result


def attribute_names(path):
    if sys.platform != "darwin":
        return os.listxattr(path, follow_symlinks=False)
    size = _xattr_call("listxattr", path, None, 0, 1)
    buffer = ctypes.create_string_buffer(size)
    used = _xattr_call("listxattr", path, buffer, size, 1)
    return [os.fsdecode(v) for v in buffer.raw[:used].split(b"\x00") if v]


def get_attribute(path, name):
    if sys.platform != "darwin":
        return os.getxattr(path, name, follow_symlinks=False)
    key = name.encode()
    size = _xattr_call("getxattr", path, key, None, 0, 0, 1)
    buffer = ctypes.create_string_buffer(size)
    used = _xattr_call("getxattr", path, key, buffer, size, 0, 1)
    return buffer.raw[:used]


def set_attribute(path, name, data):
    if sys.platform != "darwin":
        os.setxattr(path, name, data, follow_symlinks=False)
    else:
        _xattr_call("setxattr", path, name.encode(), data, len(data), 0, 1)


def relative(value):
    if not isinstance(value, str) or not value or any(c in value for c in "\x00\n\r"):
        raise ValueError("invalid app path")
    if any(p in ("", ".", "..") for p in value.split("/")):
        raise ValueError("invalid app path: " + repr(value))
    return value


def app_name(value):
    relative(value)
    if "/" in value or not value.endswith(".app") or value == ".app":
        raise ValueError("invalid app name: " + repr(value))
    return value


def contained(root, name):
    candidate = root / relative(name)
    if not candidate.resolve().is_relative_to(root.resolve()):
        raise ValueError("app path escapes its payload: " + name)
    return candidate


def load(output, name):
    app_name(name)
    manifest = output / MANIFEST
    if manifest.is_symlink() or manifest.stat().st_size > 8 * 1024 * 1024:
        raise ValueError("invalid external-signature manifest")
    data = json.loads(manifest.read_text())
    if data.get("schema") != SCHEMA or not isinstance(data.get("apps"), dict):
        raise ValueError("unsupported external-signature manifest")
    entry = data["apps"][name]
    if entry["source"] != SOURCE_DIR + "/" + name:
        raise ValueError("invalid app payload location")
    source = contained(output, entry["source"])
    if source.is_symlink() or not source.is_dir() or not (source / "Contents").is_dir():
        raise ValueError("missing app payload: " + name)
    signatures = entry["signatures"]
    if not isinstance(signatures, dict) or not signatures:
        raise ValueError("missing external signatures: " + name)
    for path, attributes in signatures.items():
        target = contained(source, path)
        if target.is_symlink() or not target.is_file():
            raise ValueError("signature target is not a regular file: " + path)
        if not isinstance(attributes, dict) or not attributes or not attributes.keys() <= ATTRS:
            raise ValueError("unsupported external signature: " + path)
        if not attributes.get("com.apple.cs.CodeDirectory"):
            raise ValueError("external signature has no CodeDirectory: " + path)
        for value in attributes.values():
            if not isinstance(value, str) or len(value) > 2 * 1024 * 1024:
                raise ValueError("invalid external signature data")
            if bytes.fromhex(value).hex() != value:
                raise ValueError("invalid external signature encoding")
    return source, signatures


def cache_path(output, name):
    app_name(name)
    home = os.environ.get("HOME")
    if not home or not Path(home).is_absolute():
        raise ValueError("HOME must be an absolute directory")
    return Path(home) / "Library/Caches/pkg/cask-apps" / output.name / name


def owned_directory(path):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise ValueError("refusing an unsafe app cache directory: " + str(path))


def cache_root(output, name):
    dest = cache_path(output, name)
    home = Path(os.environ["HOME"])
    owned_directory(home)
    current = home
    for component in ("Library", "Caches", "pkg", "cask-apps"):
        current /= component
        try:
            current.mkdir(mode=0o700)
        except FileExistsError:
            pass
        owned_directory(current)
    return dest


@contextlib.contextmanager
def locked_output(output, dest):
    # Locks live outside the version directory. A pre-existing version
    # without our exact ownership marker must never be adopted.
    lock = dest.parent.parent / (output.name + ".lock")
    fd = os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError("refusing an unsafe app cache lock")
        fcntl.flock(fd, fcntl.LOCK_EX)
        marker = dest.parent / ".pkg-owned"
        expected = SCHEMA + "\n" + str(output) + "\n"
        try:
            dest.parent.mkdir(mode=0o700)
        except FileExistsError:
            owned_directory(dest.parent)
            if marker.is_symlink() or not marker.is_file() or marker.read_text() != expected:
                raise ValueError("refusing an unowned app cache: " + str(dest.parent))
        else:
            marker.write_text(expected)
        yield
    finally:
        os.close(fd)


def signature_check(path, deep=False):
    args = ["/usr/bin/codesign", "--verify", "--strict"]
    if deep:
        args.append("--deep")
    result = subprocess.run(args + [str(path)], capture_output=True, text=True, check=False)
    if result.returncode:
        raise ValueError("vendor signature verification failed: " + result.stderr.strip()[:500])


def digest(path):
    value = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            value.update(block)
    return value.digest()


def verify(bundle, signatures, source):
    owned_directory(bundle)
    # A vendor updater could replace a cache with another validly signed
    # version. Anchor its main executable and sealed plist to this output;
    # verification alone would otherwise accept a different package version.
    info = "Contents/Info.plist"
    if digest(contained(source, info)) != digest(contained(bundle, info)):
        raise ValueError("restored app differs from its Nix payload")
    with open(source / info, "rb") as f:
        executable = plistlib.load(f).get("CFBundleExecutable")
    if not isinstance(executable, str) or "/" in relative(executable):
        raise ValueError("app has no supported main executable")
    main = "Contents/MacOS/" + executable
    if digest(contained(source, main)) != digest(contained(bundle, main)):
        raise ValueError("restored app executable differs from its Nix payload")
    for path, attributes in signatures.items():
        target = contained(bundle, path)
        if target.is_symlink() or not target.is_file():
            raise ValueError("invalid restored signature target: " + path)
        for key, value in attributes.items():
            if get_attribute(target, key) != bytes.fromhex(value):
                raise ValueError("restored vendor signature differs: " + path)
        signature_check(target)
    signature_check(bundle, deep=True)


def remove_copy(path):
    # Only called on a copy inside our marker-verified version directory.
    # Make directories writable without following any links. Store modes
    # are read-only, so ordinary rmtree would fail on nested directories.
    if path.is_symlink():
        raise ValueError("refusing a symlinked app copy: " + str(path))
    for root, _dirs, _files in os.walk(path, followlinks=False):
        os.chmod(root, 0o700)
    shutil.rmtree(path)


def prepare(output, name):
    if sys.platform != "darwin":
        raise ValueError("external-signature apps require macOS")
    source, signatures = load(output, name)
    dest = cache_root(output, name)
    with locked_output(output, dest):
        if os.path.lexists(dest):
            owned_directory(dest)  # Refuse symlink/file replacements, never remove them.
            try:
                verify(dest, signatures, source)
                return dest
            except (ValueError, OSError):
                pass  # Repair a damaged owned copy from immutable store data.
        staging = Path(tempfile.mkdtemp(prefix=".prepare-", dir=dest.parent))
        candidate = staging / name
        previous = staging / "previous.app"
        try:
            shutil.copytree(source, candidate, symlinks=True)
            for path, attributes in signatures.items():
                target = contained(candidate, path)
                mode = stat.S_IMODE(target.stat().st_mode)
                os.chmod(target, mode | stat.S_IWUSR)
                try:
                    for key, value in attributes.items():
                        set_attribute(target, key, bytes.fromhex(value))
                finally:
                    os.chmod(target, mode)
            verify(candidate, signatures, source)
            # Darwin requires write permission on a directory being moved
            # between parents. Nix store directories have mode 0555.
            mode = stat.S_IMODE(candidate.stat().st_mode)
            os.chmod(candidate, mode | stat.S_IWUSR)
            if dest.exists():
                os.chmod(dest, stat.S_IMODE(dest.stat().st_mode) | stat.S_IWUSR)
                os.rename(dest, previous)
            try:
                os.rename(candidate, dest)
            except BaseException:
                if previous.exists() and not os.path.lexists(dest):
                    os.rename(previous, dest)
                raise
            os.chmod(dest, mode)
            return dest
        finally:
            if staging.exists():
                remove_copy(staging)


def interrupted(sig, _frame):
    raise KeyboardInterrupt(sig)


def main():
    signal.signal(signal.SIGTERM, interrupted)
    output = Path(__file__).resolve().parents[2]
    try:
        mode, name = sys.argv[1:3]
        if mode == "path":
            load(output, name)
            print(cache_path(output, name))
        elif mode == "prepare":
            print(prepare(output, name))
        elif mode == "exec":
            bundle = prepare(output, name)
            executable = contained(bundle, sys.argv[3])
            if not executable.is_file() or not os.access(executable, os.X_OK):
                raise ValueError("missing bundled command")
            os.execv(executable, [str(executable)] + sys.argv[4:])
        else:
            raise ValueError("unknown app preparation command")
    except KeyboardInterrupt as error:
        sys.exit(128 + (error.args[0] if error.args else signal.SIGINT))
    except shutil.Error as error:
        failures = error.args[0]
        detail = failures[0][2] if isinstance(failures, list) and failures else str(error)
        sys.stderr.write("pkg-app: could not copy app payload: " + str(detail)[:400] + "\n")
        sys.exit(1)
    except (ValueError, OSError, KeyError, IndexError, TypeError) as error:
        sys.stderr.write("pkg-app: " + str(error)[:500] + "\n")
        sys.exit(1)


if __name__ == "__main__":
    main()
