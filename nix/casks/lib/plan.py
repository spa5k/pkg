#!/usr/bin/env python3
"""Build-time helper for generic cask-plan builders (stdlib only).

The Nix derivation passes the plan as JSON; vendor strings never become
shell syntax. Subcommands:

  unpack <src> <staging> <url-name> [raw-name]  sniff + pre-validate + extract
  install <plan.json> <staging> <out>           place artifacts, audit, plist

Safety model, in order:
  1. members (names AND symlink targets) are validated BEFORE extraction;
     duplicates and conflicting types are rejected; the inert top-level
     absolute DMG shortcut (/Applications) is omitted, never dereferenced;
  2. zip/tar use stdlib extraction with parent/target realpath checks;
     xar/cpio use bsdtar (libarchive secure extraction); 7z/dmg extract
     REGULAR FILES ONLY via 7zz with link members excluded, then links are
     re-created manually after validation (-snld is NOT a safety flag:
     upstream sets link check level 9; -snld20 disables checks entirely);
  3. after install, every output symlink (file or directory) must resolve
     inside the output (realpath + commonpath, chains resolved).
"""

import contextlib
import gzip
import importlib.util
import json
import os
import plistlib
import shutil
import shlex
import stat
import subprocess
import sys
import tarfile
import zipfile

MAX_NESTING_DEPTH = 4
MAX_LINK_HOPS = 64
MAX_LINK_TARGET = 4096  # PATH_MAX bound; macOS symlink targets are < 1024
# HFS+ DMG symlinks read via '7zz x -so' carry the target (exactly the
# listed Size bytes) plus a fixed-shape binary trailer: 01 02 00 plus 8
# volume-specific bytes. Only that exact shape is stripped; any other
# bytes after the target fail closed.
HFS_LINK_TRAILER_LEN = 11
HFS_LINK_TRAILER_HEAD = b"\x01\x02\x00"
# 7zz materializes Apple extended-attribute streams as names such as
# "Info.plist:com.apple.provenance", including inside nested bundles.
APPLE_STREAM_MARK = ":com.apple."
# Code-signing xattrs (CodeDirectory, CodeEntitlements, CodeRequirements,
# CodeRequirements-1, CodeSignature) carry a vendor's EXTERNAL code
# signature. 7zz materializes them as "<file>:com.apple.cs.<attr>". The
# Nix cannot retain them. Supported app signatures are serialized as
# ordinary data and restored outside the store; malformed streams fail.
APPLE_CS_STREAM_MARK = ":com.apple.cs."
SIGNATURE_ATTRS = frozenset(
    "com.apple.cs." + name for name in (
        "CodeDirectory", "CodeEntitlements", "CodeRequirements",
        "CodeRequirements-1", "CodeSignature",
    )
)


def _is_codesign_stream(name):
    """True when ANY path component carries a materialized Apple
    code-signing stream (7zz names them '<file>:com.apple.cs.<attr>')."""
    return any(APPLE_CS_STREAM_MARK in part for part in name.split("/"))


def _signature_stream(name):
    if not _is_codesign_stream(name):
        return None
    base, mark, suffix = name.rpartition(APPLE_CS_STREAM_MARK)
    attribute = "com.apple.cs." + suffix
    if not mark or _is_codesign_stream(base) or "/" in suffix or attribute not in SIGNATURE_ATTRS:
        die(f"unsupported external signature stream: {name}")
    if not any(p.endswith(".app") and p != ".app" for p in base.split("/")[:-1]):
        die(f"external signature outside an app bundle: {name}")
    return base, attribute


def _prune_apple_streams(dest):
    """Remove materialized Apple streams before validated links are made.

    Do not follow symlinks or remove unrelated colon names. Prune removed
    directories from the walk. A failed removal must fail the build.
    Valid signing streams remain until app installation captures them.
    """
    for dirpath, dirnames, filenames in os.walk(dest, followlinks=False):
        for name in dirnames[:]:
            path = os.path.join(dirpath, name)
            if _is_codesign_stream(name):
                die(f"signature stream is not a regular file: {path}")
            if APPLE_STREAM_MARK in name and not os.path.islink(path):
                shutil.rmtree(path)
                dirnames.remove(name)
        for name in filenames:
            path = os.path.join(dirpath, name)
            if _is_codesign_stream(name):
                _signature_stream(os.path.relpath(path, dest))
                if os.path.islink(path):
                    die(f"signature stream is not a regular file: {path}")
                continue
            if APPLE_STREAM_MARK in name and not os.path.islink(path):
                os.remove(path)


def resolve_graph(links, path):
    """Resolve relative `path` against the symlink map `links` (member
    name -> target). Components are substituted BEFORE any `..` is
    applied, so composed links (a -> b/.. with b -> .) cannot escape:
    `..` above the resolved root dies. Bounded hop budget kills cycles.
    Returns the resolved in-tree path."""
    parts, resolved, hops = path.split("/") if path else [], [], 0
    guard = 0
    while parts:
        guard += 1
        if guard > MAX_LINK_HOPS * MAX_NESTING_DEPTH:
            die(f"symlink resolution exceeds hop budget: {path}")
        head, parts = parts[0], parts[1:]
        if head in ("", "."):
            continue
        if head == "..":
            if not resolved:
                die(f"member resolves above staging root: {path}")
            resolved.pop()
            continue
        candidate = "/".join(resolved + [head])
        if candidate in links:
            hops += 1
            if hops > MAX_LINK_HOPS:
                die(f"symlink resolution exceeds hop budget: {path}")
            target = links[candidate]
            if target.startswith("/"):
                die(f"reject absolute vendor link: {candidate} -> {target}")
            parts = target.split("/") + parts  # keep '..' for after
            continue
        resolved.append(head)
    return "/".join(resolved)


def die(msg):
    sys.stderr.write(f"cask-build: {msg}\n")
    sys.exit(1)


def run(cmd, **kw):
    # returncode is checked manually right below (check=False is explicit)
    p = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="surrogateescape",
        check=False,
        **kw,
    )
    if p.returncode != 0:
        die(
            f"command failed ({cmd[0]} {cmd[1] if len(cmd) > 1 else ''}): "
            f"{p.stderr.strip()[:300]}"
        )
    return p.stdout


def run_bytes(cmd, **kw):
    # byte mode for streams that are not UTF-8 text (HFS+ -so link data)
    p = subprocess.run(cmd, capture_output=True, check=False, **kw)
    if p.returncode != 0:
        err = p.stderr.decode("utf-8", "replace").strip()[:300]
        die(
            f"command failed ({cmd[0]} {cmd[1] if len(cmd) > 1 else ''}): "
            f"{err}"
        )
    return p.stdout


# ------------------------------------------------------------- path rules


def safe_rel_path(p, what, allow_dir=False):
    """Strict relative path; trailing '/' allowed for directory members."""
    if not p or "\x00" in p or "\n" in p or p.startswith("/"):
        die(f"unsafe {what}: {p!r}")
    norm = p[:-1] if allow_dir and p.endswith("/") else p
    parts = norm.split("/")
    if any(part in ("", "..") for part in parts):
        die(f"unsafe {what}: {p!r}")
    return norm


def inside(root, candidate):
    root = os.path.realpath(root)
    cand = os.path.realpath(candidate)
    return cand == root or os.path.commonpath([root, cand]) == root


def link_target_ok(rel_dir, target):
    """Relative symlink target stays inside the tree (legit in-bundle
    '..' chains that land back inside are allowed)."""
    if target.startswith("/"):
        return False
    base = os.path.normpath(os.path.join(rel_dir, target))
    return base == "." or (base != ".." and not base.startswith("../"))


def _norm_dots(name):
    """Drop harmless '.' path segments ('./a' -> 'a') so graph keys, seen,
    existing and omit sets share one canonical form; a lone '.'/'./' is the
    root directory. '..' is rejected by safe_rel_path, never here."""
    return "/".join(p for p in name.split("/") if p != ".")


def _member_key(raw):
    """Canonical member key: validate_members, the omit set, and every
    extractor comparison share it, so stored spellings like
    './Applications' and 'Applications' are the SAME member everywhere
    (bsdtar/cpio and tar list names exactly as stored)."""
    return _norm_dots(safe_rel_path(raw, "archive member", allow_dir=True))


def validate_members(members):
    """members: iterable of dicts {name, is_symlink, link_target, is_dir}.
    Validates the COMPLETE symlink graph before any payload write: names,
    duplicates, type conflicts, special members (fifo/device), hard links
    (fail closed, same policy as 7zz), and every member destination
    resolved with
    symlink substitution (composed escapes and cycles die). Returns the
    omit set: the conventional DMG /Applications shortcut at the archive
    root or directly under a single volume-wrapper root, outside any .app.
    Legitimate framework chains (Versions/Current -> A) resolve and pass."""
    links, omit, seen, dests = {}, set(), {}, set()
    names = []
    for m in members:
        name = _member_key(m["name"])
        if name == "":
            # the root directory member ('.'/'./', common in cpio)
            if not m.get("is_dir"):
                die(f"unsafe archive member: {m['name']!r}")
            seen[""] = "d"
            continue
        names.append((name, m))
    # pass 1: names, duplicates, absolute/escaping targets, omit set, graph
    for name, m in names:
        kind = "d" if m.get("is_dir") else ("l" if m["is_symlink"] else "f")
        if name in seen and seen[name] != kind:
            die(f"conflicting archive member types for: {name}")
        if name in seen and kind != "d":
            die(f"duplicate archive member: {name}")
        seen[name] = kind
        if m.get("is_special"):
            die(f"unsupported special archive member (fifo/device): {name}")
        if m.get("hard_target") is not None:
            # tar hard links are refused pre-write, same policy as 7zz. A
            # hard-link graph (chains, cycles, targets routed through
            # symlink members) is the composed-path class resolve_graph
            # covers for symlinks; validating it needs a second resolver.
            # This builder does not support hard links. Fail closed.
            die(f"unsupported hard link member: {name} -> {m['hard_target']}")
        if m["is_symlink"]:
            t = m["link_target"] or ""
            if not t:
                # a real symlink always has a nonempty target; empty means
                # a corrupt/lying record and must fail closed, not pass as
                # a zero-length link (resolves to its own parent dir)
                die(f"empty link target for: {name}")
            if t.startswith("/"):
                parts = name.split("/")
                at_shortcut_root = (
                    len(parts) <= 2
                    and parts[-1] == "Applications"
                    and t == "/Applications"
                    and not any(p.endswith(".app") for p in parts[:-1])
                )
                if at_shortcut_root and not m.get("is_dir"):
                    omit.add(name)
                    continue
                die(f"reject absolute vendor link: {name} -> {t}")
            links[name] = t
    existing = set(seen)
    for name, m in names:
        stream = _signature_stream(name)
        if stream:
            base, _attribute = stream
            if seen.get(name) != "f" or seen.get(base) != "f":
                die(f"signature stream needs a regular file target: {name}")
            if m.get("size") is not None and m["size"] > 1024 * 1024:
                die(f"signature stream is too large: {name}")
    for name in list(existing):
        parts = name.split("/")
        for i in range(1, len(parts)):
            existing.add("/".join(parts[:i]))  # implicit parent dirs
    # pass 2: full graph resolution of every member and link destination
    for name, m in names:
        if name in omit:
            continue
        resolved = resolve_graph(links, name)
        if not m["is_symlink"]:  # only writers can collide on a destination
            if resolved in dests:
                die(f"duplicate resolved destination: {name} -> {resolved}")
            dests.add(resolved)
        else:
            dest = resolve_graph(
                links, os.path.join(os.path.dirname(name), links[name])
            )
            if dest and dest not in existing:
                die(f"dangling link in archive: {name} -> {links[name]}")
    return omit


# ------------------------------------------------------- listing adapters


def zip_members(zf):
    out = []
    for zi in zf.infolist():
        mode = zi.external_attr >> 16
        is_link = stat.S_ISLNK(mode)
        link_target = None
        if is_link:
            # a symlink-flagged member is a payload read, so bound it BEFORE
            # reading: an oversized "target" is a decompression bomb, and
            # control characters mean a corrupt link (same rule as 7zz)
            if zi.file_size > MAX_LINK_TARGET:
                die(f"symlink member target too large: {zi.filename}")
            link_target = zf.read(zi).decode("utf-8", "replace")
            if any(c in link_target for c in "\x00\n\r"):
                die(f"corrupt link target bytes for: {zi.filename}")
        out.append(
            {
                "name": zi.filename,
                "is_symlink": is_link,
                "is_dir": zi.is_dir(),
                "link_target": link_target,
            }
        )
    return out


def tar_members(tf):
    return [
        {
            "name": m.name,
            "is_symlink": m.issym(),
            "is_dir": m.isdir(),
            "is_special": m.isfifo() or m.isdev(),
            "hard_target": m.linkname if m.islnk() else None,
            "link_target": m.linkname if m.issym() else None,
        }
        for m in tf.getmembers()
    ]


GLOB_META = set("*?[]")


def bsdtar_members(archive):
    """Strict listing for xar/cpio via `bsdtar -tvf` (stable 8-column
    format) and raw 7z/DMG records via `7zz l -ba -slt`. Unparseable
    output fails clear; nothing is silently skipped."""
    if _is_7z_family(archive):
        return sevenz_members(archive), None
    import re

    # LC_ALL=C format: perms links owner group size mmm DD (HH:MM|YYYY) name[ -> target]
    row = re.compile(
        r"^([dl-][rwxsStT-]{9})\s+\d+\s+\S+\s+\S+\s+\d+\s+"
        r"[A-Z][a-z]{2}\s+\d{1,2}\s+(?:\d{2}:\d{2}|\d{4})\s+(.+)$"
    )
    env = dict(os.environ, LC_ALL="C")
    # returncode is checked manually right below (check=False is explicit)
    p = subprocess.run(
        ["bsdtar", "-tv", "-f", archive],
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )
    if p.returncode != 0:
        die(f"bsdtar listing failed: {p.stderr.strip()[:200]}")
    out = []
    for lineno, line in enumerate(p.stdout.splitlines(), 1):
        m = row.match(line)
        if not m:
            die(f"unparseable bsdtar listing line {lineno}: {line!r}")
        rest = m.group(2).strip()
        if m.group(1).startswith("l"):
            if " -> " not in rest:
                die(f"unparseable bsdtar symlink line {lineno}: {line!r}")
            name, target = rest.split(" -> ", 1)
            out.append(
                {
                    "name": name,
                    "is_symlink": True,
                    "is_dir": False,
                    "link_target": target.strip(),
                }
            )
        else:
            out.append(
                {
                    "name": rest,
                    "is_symlink": False,
                    "is_dir": m.group(1).startswith("d"),
                    "link_target": None,
                }
            )
    return out, validate_members(out)


def sevenz_members(archive):
    """Raw (unvalidated) member records from `7zz l -ba -slt`. Real DMG
    listings use `Folder = +/-` and `Mode = drwx.../lrwx...` records — not
    `Attributes` — and HFS+ symlinks carry `Mode = l...` with NO
    `Symbolic Link` field (their target must be read separately via
    `7zz x -so`). Hard links fail closed. Trailing CR (CRLF output) is
    stripped once; embedded CR in a name is ambiguous and rejected."""
    text = run(["7zz", "l", "-ba", "-slt", "--", archive])
    records, path, rec = [], None, {}
    for line in text.split("\n") + ["Path = "]:
        line = line.rstrip("\r")
        if line.startswith("Path = "):
            if path is not None:
                records.append((path, rec))
            path, rec = line[7:], {}
        elif line.startswith("Folder = "):
            rec["folder"] = line[9:].strip()
        elif line.startswith("Size = "):
            value = line[7:].strip()
            rec["size"] = int(value) if value.isdigit() else None
        elif line.startswith("Mode = "):
            rec["mode"] = line[7:].strip()
        elif line.startswith("Attributes = "):
            rec["attrs"] = line[13:].strip()
        elif line.startswith("Symbolic Link = "):
            rec["sym"] = line[16:]
        elif line.startswith("Hard Link = "):
            rec["hard"] = line[12:]
    out = []
    for p, r in records:
        if not p or "\r" in p or "\n" in p:
            die(f"unparseable 7zz listing entry: {p!r}")
        _signature_stream(p)
        if r.get("hard"):
            # APFS records also emit `Hard Link = ` with an EMPTY value for
            # ordinary members; only a nonempty value is a real hard link
            die(f"unsupported hard link member: {p}")
        mode = r.get("mode", "")
        # -snl archives on Linux carry the POSIX mode in Attributes
        # ("A lrwxrwxrwx"), with no Mode/Symbolic Link field; take the
        # last attribute token as the mode when it looks like one
        attr_tokens = r.get("attrs", "").replace("_", " ").split()
        posix = attr_tokens[-1] if attr_tokens else ""
        # APFS listings emit `Symbolic Link = ` with an EMPTY value for
        # ordinary dirs/files too; the Mode (d.../-r...) is authoritative.
        # A link is: NONEMPTY Symbolic Link, or Mode/Attributes 'l...'.
        # An empty value is treated as absent, so dirs/files are never
        # misread as dangling zero-length links. mode=l with an empty
        # value (HFS+) keeps link_target=None for the `-so` byte read.
        sym = r.get("sym", "")
        is_link = bool(sym) or mode.startswith("l") or posix.startswith("l")
        is_dir = not is_link and (
            r.get("folder") == "+"
            or mode.startswith("d")
            or posix.startswith("d")
            or "D" in attr_tokens
        )
        out.append(
            {
                "name": p,
                "is_symlink": is_link,
                "is_dir": is_dir,
                "link_target": sym or None,
                "size": r.get("size"),
            }
        )
    return out


def _7z_link_target(archive, name, size=None):
    """Read a symlink's target bytes via '7zz x -so' (stdout only, no
    filesystem extraction). 7zz name arguments are wildcard patterns, so
    members containing glob metacharacters fail closed.

    HFS+ DMG streams carry the target plus a fixed 11-byte binary trailer
    (01 02 00 plus 8 volume-specific bytes). The listing's Size is the
    true target length -- the same value plain 7zz filesystem extraction
    uses when it creates correct links -- so exactly Size bytes are kept
    and the trailer is stripped only when its length and head match. Any
    other remainder, non-UTF-8 bytes, or control characters in the
    target fail closed."""
    if GLOB_META & set(name):
        die(f"cannot safely read link target with glob metacharacters: {name}")
    data = run_bytes(["7zz", "x", "-so", "--", archive, name])
    if size is None:
        if len(data) > MAX_LINK_TARGET:
            die(f"symlink member target too large: {name}")
    else:
        if size > MAX_LINK_TARGET:
            die(f"symlink member target too large: {name}")
        if len(data) < size:
            die(f"short link target stream for: {name}")
        extra = data[size:]
        if extra and not (
            len(extra) == HFS_LINK_TRAILER_LEN
            and extra.startswith(HFS_LINK_TRAILER_HEAD)
        ):
            die(f"unexpected bytes after link target for: {name}")
        data = data[:size]
    try:
        target = data.decode("utf-8")
    except UnicodeDecodeError:
        die(f"non-UTF-8 link target bytes for: {name}")
    if not target:
        die(f"empty link target for: {name}")
    if any(c in target for c in "\r\n\x00"):
        die(f"corrupt link target bytes for: {name}")
    return target


def _is_7z_family(path):
    return _mime(path) == "application/x-7z-compressed" or is_udif(path)


def is_udif(path):
    """Content-based DMG detection: the UDIF trailer is a 512-byte block
    starting with `koly` at end of file. `file` can misreport DMGs as
    `application/zlib`, so the trailer is authoritative."""
    try:
        with open(path, "rb") as f:
            f.seek(-512, os.SEEK_END)
            return f.read(4) == b"koly"
    except OSError:
        return False


def _mime(path):
    return run(["file", "--mime-type", "-b", "--", path]).strip()


# ------------------------------------------------------------- extractors


def _check_target_path(dest, name):
    """Parent AND final path must stay inside dest (no writing through a
    previously created symlink, no ancestor escape). Boundary checks run
    BEFORE any directory creation."""
    target = os.path.join(dest, name)
    if os.path.lexists(target) and not os.path.isdir(target):
        die(f"duplicate/conflicting extraction path: {name}")
    parent = os.path.dirname(target)
    if not inside(dest, parent) or not inside(dest, target):
        die(f"extraction path escapes staging: {name}")
    os.makedirs(parent, exist_ok=True)


def extract_zip(src, dest):
    with zipfile.ZipFile(src) as zf:
        omit = validate_members(zip_members(zf))
        for zi in zf.infolist():
            name = safe_rel_path(zi.filename, "zip member", allow_dir=True)
            if _norm_dots(name) in omit:  # canonical: './Applications' too
                continue
            _check_target_path(dest, name)
            target = os.path.join(dest, name)
            mode = zi.external_attr >> 16
            if stat.S_ISLNK(mode):
                os.makedirs(os.path.dirname(target), exist_ok=True)
                lt = zf.read(zi).decode("utf-8", "replace")
                if not link_target_ok(os.path.dirname(name), lt):
                    die(f"reject escaping vendor link: {name} -> {lt}")
                os.symlink(lt, target)
            elif zi.is_dir():
                os.makedirs(target, exist_ok=True)
            else:
                with zf.open(zi) as f, open(target, "wb") as g:
                    shutil.copyfileobj(f, g)
                os.chmod(target, 0o755 if mode & 0o111 else 0o644)


def extract_tar(src, dest):
    with tarfile.open(src) as tf:
        omit = validate_members(tar_members(tf))
        members = [m for m in tf.getmembers() if _member_key(m.name) not in omit]
        tf.extractall(dest, members=members, filter="data")


def extract_bsdtar(src, dest):
    """xar/cpio: libarchive secure extraction refuses absolute and
    '..' members by default; inert shortcuts are excluded by their RAW
    stored name ('./Applications'), matched via the canonical key."""
    members, omit = bsdtar_members(src)
    excludes = []
    for m in members:
        raw = m["name"].rstrip("/")
        if _member_key(m["name"]) in omit:
            if GLOB_META & set(raw):
                die(f"cannot safely exclude member with glob metacharacters: {raw}")
            excludes += ["--exclude", raw]
    run(["bsdtar", "-x", "-f", src, "-C", dest] + excludes)


def extract_7zz(src, dest):
    """7z/DMG: extract REGULAR FILES AND DIRECTORIES ONLY. EVERY link
    member (including the omitted /Applications shortcut) is excluded
    from 7zz (its link handling is not a safety boundary); targets are
    read safely via `7zz x -so`, validated BEFORE extraction, and links
    are re-created manually afterwards."""
    members = sevenz_members(src)
    for m in members:
        if m["is_symlink"] and m["link_target"] is None:
            m["link_target"] = _7z_link_target(src, m["name"], m.get("size"))
    omit = validate_members(members)
    excludes, links = [], {}
    for m in members:
        name = m["name"].rstrip("/")
        if _norm_dots(name) in omit or m["is_symlink"]:
            # only names handed to 7zz as exclusion/selection patterns need
            # the glob guard; plain extracted members keep their exact names
            # (real DMGs carry bracketed HFS+ metadata folders)
            if GLOB_META & set(m["name"]):
                die(
                    f"cannot safely exclude member with glob metacharacters: {m['name']}"
                )
            if m["is_symlink"] and name not in omit:
                links[name] = m["link_target"]
            excludes += ["-x!" + m["name"]]
    # switches must come BEFORE `--`: 7zz stops switch parsing at `--`,
    # so `-x!` after the archive would act as an extract-only name filter
    # and silently drop every other member (verified on a real DMG)
    run(["7zz", "x", f"-o{dest}"] + excludes + ["--", src])
    # AppleDouble/xattr stream remnants must not survive ANYWHERE in the
    # extracted tree: a signed bundle fails codesign --verify when a
    # stray "PkgInfo:com.apple.provenance" sits next to the real PkgInfo
    # at a nested path (the old direct-children-only loop left them)
    _prune_apple_streams(dest)
    for name, target in sorted(links.items()):
        _check_target_path(dest, name)
        if not link_target_ok(os.path.dirname(name), target):
            die(f"reject escaping vendor link: {name} -> {target}")
        os.symlink(target, os.path.join(dest, name))


def stage_payload(src, staging, declared, url_name, default, strip_suffix=""):
    """Copy a whole-file payload under its VALIDATED DECLARED relative
    path (artifact source), creating parents; the URL basename is only a
    fallback when no artifact source was declared."""
    name = (declared or url_name or default).split("?")[0]
    if strip_suffix and name.endswith(strip_suffix):
        name = name[: -len(strip_suffix)]
    safe_rel_path(name, "staged payload name")
    dest = os.path.join(staging, name)
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    shutil.copy(src, dest)


def cmd_unpack(src, staging, url_name, raw_name="", fallback_name=""):
    os.makedirs(staging, exist_ok=True)
    if raw_name:
        stage_payload(src, staging, raw_name, "", "payload.bin")
        return
    if is_udif(src):
        # content-based UDIF dispatch: `file` reports some DMGs as
        # application/zlib, so the koly trailer decides, not the MIME type
        extract_7zz(src, staging)
        return
    mime = _mime(src)
    if zipfile.is_zipfile(src):
        extract_zip(src, staging)
    elif mime == "application/x-xar":
        # A bare xar IS a pkg; keep the file, the pkg step unpacks it.
        base = (fallback_name or url_name or "payload.pkg").split("?")[0]
        shutil.copy(src, os.path.join(staging, os.path.basename(base)))
    elif _is_7z_family(src):
        extract_7zz(src, staging)
    elif mime in (
        "application/x-tar",
        "application/gzip",
        "application/x-bzip2",
        "application/x-xz",
        "application/zstd",
        "application/x-lzma",
    ):
        try:
            extract_tar(src, staging)
        except tarfile.TarError:
            if mime == "application/gzip":
                # single gzipped binary: the declared artifact source names
                # the file; URL basenames can carry query params or no hint
                stage_payload(
                    src,
                    staging,
                    fallback_name,
                    url_name,
                    "payload.bin",
                    strip_suffix=".gz",
                )
            else:
                die(f"unresolved archive format (mime: {mime})")
    elif mime in (
        "application/x-executable",
        "application/x-pie-executable",
        "application/x-mach-binary",
        "application/x-sharedlib",
        "text/x-shellscript",
        "application/octet-stream",
    ):
        stage_payload(src, staging, fallback_name, url_name, "payload.bin")
    else:
        die(f"unresolved archive format (mime: {mime}, url: {url_name})")


# ---------------------------------------------------------------- install

COMP_DIRS = {
    "bash-completion": "share/bash-completion/completions",
    "zsh-completion": "share/zsh/site-functions",
    "fish-completion": "share/fish/vendor_completions.d",
}
KNOWN_KINDS = {
    "app",
    "binary",
    "pkg",
    "appimage",
    "manpage",
    "bash-completion",
    "zsh-completion",
    "fish-completion",
}


def locate(staging, name):
    safe_rel_path(name, "artifact source")
    if GLOB_META & set(name):
        die(f"artifact source with glob metacharacters unsupported: {name!r}")
    direct = os.path.join(staging, name)
    if os.path.lexists(direct):
        return direct
    # exact relative suffix under volume nesting (find -name never
    # matches '/' and treats the name as a glob); ambiguous hits die.
    # find failures surface as zero hits and die below (check=False is
    # explicit; the return code is not inspected).
    hits = subprocess.run(
        [
            "find",
            staging,
            "-mindepth",
            "2",
            "-maxdepth",
            str(MAX_NESTING_DEPTH + name.count("/") + 2),
            "-path",
            os.path.join(staging, "*", name),
            "-print",
        ],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.splitlines()
    hits = [h for h in hits if h]
    if len(hits) > 1:
        die(f"ambiguous artifact source, multiple matches for: {name}")
    if not hits:
        die(f"declared artifact not found in vendor archive: {name}")
    return hits[0]


def plist_load(path):
    with open(path, "rb") as f:
        try:
            return plistlib.load(f)  # XML and binary plists, read-only
        except Exception as e:
            die(f"unreadable plist {path}: {e}")


def plist_min_ok(bundle, baseline):
    p = os.path.join(bundle, "Contents", "Info.plist")
    if not os.path.isfile(p):
        return
    req = plist_load(p).get("LSMinimumSystemVersion")
    if not req:
        return
    try:
        nums = tuple(int(x) for x in (str(req).split(".") + [0, 0, 0])[:3])
    except ValueError:
        die(f"malformed LSMinimumSystemVersion {req!r} in {bundle}")
    if nums > baseline:
        die(
            f"{os.path.basename(bundle)} requires macOS {req}, "
            f"baseline is {'.'.join(map(str, baseline))}"
        )


def one_component(name, what):
    if "/" in name or name in ("", ".", "..") or "\x00" in name:
        die(f"{what} must be a single path component: {name!r}")
    return name


def elf_ok_for_linux(path, arch):
    with open(path, "rb") as f:
        head = f.read(20)
    if head[:4] == b"\x7fELF":
        machine = int.from_bytes(head[18:20], "little")
        want = {"x86_64-linux": 62, "aarch64-linux": 183}.get(arch)
        if want is None or machine != want:
            die(
                f"binary {os.path.basename(path)} is ELF for another "
                f"architecture (machine {machine}, expected {want} for {arch})"
            )
        return
    if head[:4] in (
        b"\xfe\xed\xfa\xce",
        b"\xfe\xed\xfa\xcf",
        b"\xcf\xfa\xed\xfe",
        b"\xce\xfa\xed\xfe",
    ):
        die(f"binary {os.path.basename(path)} is a Mach-O payload; not a Linux binary")
    if head[:2] == b"MZ":
        die(
            f"binary {os.path.basename(path)} is a Windows PE payload; "
            "not a Linux binary"
        )
    if head[:2] == b"#!":
        return  # bounded script format
    die(f"binary {os.path.basename(path)} is not ELF, script, or known payload")


def audit_output(out):
    for dirpath, dirnames, filenames in os.walk(out):
        for n in dirnames + filenames:
            p = os.path.join(dirpath, n)
            if not os.path.islink(p):
                continue
            t = os.readlink(p)
            resolved = os.path.realpath(p)  # follows the whole chain
            if not os.path.lexists(resolved):
                die(f"dangling link in output: {p} -> {t}")
            if not inside(out, p):
                die(f"vendor link escapes output: {p} -> {t}")


def capture_signatures(bundle, runtime):
    """Serialize external signatures before Python or Nix can drop EAs.

    Explicit 7zz streams and native attributes share the same contract.
    Only regular in-bundle files and the known Apple signing names pass.
    """
    signatures = {}
    native = None
    if sys.platform == "darwin":
        source = runtime["source"] if runtime else os.path.join(os.path.dirname(__file__), "app-runtime.py")
        spec = importlib.util.spec_from_file_location("pkg_app_runtime", source)
        native = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(native)
        if any(a.startswith("com.apple.cs.") for a in native.attribute_names(bundle)):
            die("external signature on an app directory is unsupported")

    def add(rel, attribute, value):
        path = os.path.join(bundle, safe_rel_path(rel, "signature target"))
        if os.path.islink(path) or not os.path.isfile(path) or not inside(bundle, path):
            die(f"signature target is not a regular in-bundle file: {rel}")
        if (attribute not in SIGNATURE_ATTRS or len(value) > 1024 * 1024
                or (attribute == "com.apple.cs.CodeDirectory" and not value)):
            die(f"unsupported external signature: {rel}:{attribute}")
        attrs = signatures.setdefault(rel, {})
        encoded = value.hex()
        if attribute in attrs and attrs[attribute] != encoded:
            die(f"conflicting external signature: {rel}:{attribute}")
        attrs[attribute] = encoded

    for root, dirs, files in os.walk(bundle, followlinks=False):
        for name in dirs + files:
            path = os.path.join(root, name)
            rel = os.path.relpath(path, bundle)
            if _is_codesign_stream(rel):
                stream = _signature_stream(os.path.basename(bundle) + "/" + rel)
                if os.path.islink(path) or not os.path.isfile(path):
                    die(f"signature stream is not a regular file: {rel}")
                with open(path, "rb") as f:
                    value = f.read(1024 * 1024 + 1)
                base, attribute = stream
                add(base.split("/", 1)[1], attribute, value)
            elif native:
                for attribute in native.attribute_names(path):
                    if attribute.startswith("com.apple.cs."):
                        add(rel, attribute, native.get_attribute(path, attribute))
    for rel, attrs in signatures.items():
        if "com.apple.cs.CodeDirectory" not in attrs:
            die(f"external signature has no CodeDirectory: {rel}")
    return signatures


def copy_app(source, dest, ctx, signed):
    signatures = capture_signatures(source, ctx.get("appRuntime"))
    if signatures and not ctx.get("appRuntime"):
        die("cannot preserve external signatures without the app runtime")
    shutil.copytree(source, dest, symlinks=True)
    if signatures:
        for root, _dirs, files in os.walk(dest, followlinks=False):
            for name in files:
                if _is_codesign_stream(name):
                    os.remove(os.path.join(root, name))
        signed[os.path.basename(dest)] = signatures


def pack_signed_apps(ctx, out, signed):
    if not signed:
        return
    runtime = ctx["appRuntime"]
    base = os.path.join(out, "libexec", "pkg")
    source_dir = os.path.join(base, "app-sources")
    os.makedirs(source_dir, exist_ok=True)
    manifest = {"schema": "pkg-cask-apps/1", "apps": {}}
    for name, signatures in sorted(signed.items()):
        os.rename(os.path.join(out, "Applications", name), os.path.join(source_dir, name))
        manifest["apps"][name] = {
            "source": f"libexec/pkg/app-sources/{name}",
            "signatures": signatures,
        }
    data_dir = os.path.join(out, "share", "pkg")
    os.makedirs(data_dir, exist_ok=True)
    with open(os.path.join(data_dir, "cask-apps.json"), "w") as f:
        encoded = json.dumps(manifest, sort_keys=True)
        if len(encoded.encode()) > 8 * 1024 * 1024:
            die("external-signature manifest is too large")
        f.write(encoded)
    script = os.path.join(base, "cask-app.py")
    shutil.copyfile(runtime["source"], script)
    helper = os.path.join(out, "libexec", "pkg-cask-app")
    with open(helper, "w") as f:
        f.write("#!/bin/sh\nexec " + shlex.quote(runtime["python"]) + " " + shlex.quote(script) + ' "$@"\n')
    os.chmod(helper, 0o555)


def install_pkg(ctx, plan, staging, out, baseline, signed):
    pkg_arts = [a for a in plan["artifacts"] if a["kind"] == "pkg"]
    if len(pkg_arts) != 1:
        die(f"pkg plans need exactly one pkg artifact, got {len(pkg_arts)}")
    pkg_art = pkg_arts[0]
    if pkg_art.get("target") is not None:
        die("pkg artifact must have null target")
    pkg_path = os.path.join(staging, pkg_art["source"])
    if not os.path.isfile(pkg_path):
        # a bare XAR download is kept under the sniff display name; map the
        # single staged xar file to the declared pkg artifact source
        cands = [
            os.path.join(staging, n)
            for n in os.listdir(staging)
            if os.path.isfile(os.path.join(staging, n))
            and _mime(os.path.join(staging, n)) == "application/x-xar"
        ]
        pkg_path = cands[0] if len(cands) == 1 else locate(staging, pkg_art["source"])
    xdir = os.path.join(staging, ".xar")
    os.makedirs(xdir, exist_ok=True)
    members, _ = bsdtar_members(pkg_path)
    for m in members:
        n = m["name"].rstrip("/")
        if (
            n == "Scripts"
            or n.startswith("Scripts/")
            or n.endswith("/Scripts")
            or "/Scripts/" in n
        ):
            die(f"pkg carries installer scripts: {n}")
        if n == "Distribution" or "/Distribution" in n or n.endswith("/Distribution"):
            die(
                "pkg carries Distribution installer logic; "
                "choice-requiring pkgs are unsupported"
            )
        if "Plugins" in n.split("/"):
            die(f"pkg carries installer plugins: {n}")
        if n.endswith(".pkg"):
            die(f"pkg nests component pkgs: {n}")
    extract_bsdtar(pkg_path, xdir)
    payloads = []
    for dirpath, _dirs, files in os.walk(xdir):
        for n in files:
            if n == "Payload":
                payloads.append(os.path.join(dirpath, n))
    if not payloads:
        die("pkg has no relocatable payload")
    paydir = os.path.join(staging, ".payload")
    os.makedirs(paydir, exist_ok=True)
    for payload in payloads:
        dec = payload + ".dec"
        with open(payload, "rb") as f:
            head = f.read(2)
        if head == b"\x1f\x8b":
            with gzip.open(payload, "rb") as f, open(dec, "wb") as g:
                shutil.copyfileobj(f, g)
        else:
            with open(dec, "wb") as g:  # pbzx -n writes xz stream to stdout
                subprocess.run(["pbzx", "-n", payload], stdout=g, check=True)
        extract_bsdtar(dec, paydir)  # cpio via libarchive, pre-validated
    # structural relocatable-layout rule over the merged payload;
    # system locations are rejected first, naming the component
    apps_dir = os.path.join(out, "Applications")
    for entry in sorted(os.listdir(paydir)):
        p = os.path.join(paydir, entry)
        rel = safe_rel_path(entry, "pkg payload component", allow_dir=True)
        if (
            rel == "Library"
            or rel.startswith("Library/")
            or rel == "System"
            or rel.startswith("System/")
            or rel.endswith(".kext")
            or "LaunchDaemons" in rel
            or "LaunchAgents" in rel
            or "PrivilegedHelperTools" in rel
        ):
            die(f"pkg payload carries system component: {rel}")
        if os.path.isdir(p) and not os.path.islink(p):
            if rel == "Applications":
                for child in sorted(os.listdir(p)):
                    if not child.endswith(".app"):
                        die(
                            f"pkg Applications payload must contain only .app "
                            f"bundles, found: {child}"
                        )
                    dest = os.path.join(apps_dir, child)
                    if os.path.lexists(dest):
                        die(f"duplicate bundle in pkg payload: {child}")
                    os.makedirs(apps_dir, exist_ok=True)
                    copy_app(os.path.join(p, child), dest, ctx, signed)
            elif rel.endswith(".app"):
                os.makedirs(apps_dir, exist_ok=True)
                copy_app(p, os.path.join(apps_dir, rel), ctx, signed)
            elif rel == "Contents":
                name = one_component(
                    str(
                        plist_load(os.path.join(p, "Info.plist")).get("CFBundleName")
                        or ctx["token"]
                    ),
                    "CFBundleName",
                )
                os.makedirs(apps_dir, exist_ok=True)
                # Capture signing attributes while the original pkg payload
                # still has them. Present a normal app root to the copier.
                bundle = os.path.join(staging, ".pkg-app", f"{name}.app")
                os.makedirs(bundle, exist_ok=True)
                os.rename(p, os.path.join(bundle, "Contents"))
                copy_app(bundle, os.path.join(apps_dir, f"{name}.app"), ctx, signed)
            else:
                die(f"pkg payload component outside supported layouts: {rel}")
        else:
            die(
                f"pkg payload component outside supported layouts "
                f"(plain files unsupported): {rel}"
            )
    if os.path.isdir(apps_dir):
        for b in os.listdir(apps_dir):
            plist_min_ok(os.path.join(apps_dir, b), baseline)


def cmd_install(plan_file, staging, out):
    with open(plan_file) as f:
        ctx = json.load(f)
    plan = ctx["plan"]
    baseline = tuple(int(x) for x in (ctx["baseline"].split(".") + [0, 0, 0])[:3])
    arch = ctx["system"]
    if plan.get("archive", {}).get("kind") == "appimage":
        die("appimage plans use the appimageTools builder")
    kinds = [a["kind"] for a in plan["artifacts"]]
    unknown = [k for k in kinds if k not in KNOWN_KINDS]
    if unknown:
        die(f"unknown artifact kinds: {unknown}")
    if "appimage" in kinds:
        die("appimage artifact outside an appimage plan")

    # apps first: rename map source -> installed target, plist check
    apps_dir = os.path.join(out, "Applications")
    app_targets = {}
    signed = {}
    for a in [x for x in plan["artifacts"] if x["kind"] == "app"]:
        src = locate(staging, a["source"])
        tgt = one_component(a["target"], "app target")
        if not tgt.endswith(".app"):
            die(f"app target must be a bundle rename ending .app: {tgt!r}")
        os.makedirs(apps_dir, exist_ok=True)
        dest = os.path.join(apps_dir, tgt)
        if os.path.lexists(dest):
            die(f"duplicate app target: {tgt}")
        copy_app(src, dest, ctx, signed)
        plist_min_ok(dest, baseline)
        app_targets[os.path.basename(a["source"].rstrip("/"))] = tgt

    if "pkg" in kinds:
        install_pkg(ctx, plan, staging, out, baseline, signed)

    for a in plan["artifacts"]:
        kind = a["kind"]
        if kind in ("app", "pkg"):
            continue
        if a.get("target") is None:
            die(f"{kind} artifact needs a target")
        tgt = one_component(a["target"], f"{kind} target")
        if kind == "binary":
            src = a["source"]
            bindir = os.path.join(out, "bin")
            os.makedirs(bindir, exist_ok=True)
            dest = os.path.join(bindir, tgt)
            if os.path.lexists(dest):
                die(f"duplicate binary target: {tgt}")
            if src.startswith("$APPDIR/"):
                rest = safe_rel_path(src[len("$APPDIR/") :], "$APPDIR binary source")
                declared_app, _, inner = rest.partition("/")
                installed = app_targets.get(declared_app, declared_app)
                if installed not in app_targets.values():
                    die(
                        f"$APPDIR binary {src} does not name a declared app "
                        "(bundle source or renamed target required)"
                    )
                resolved = os.path.join(apps_dir, installed, inner)
                if not os.path.lexists(resolved):
                    die(f"$APPDIR binary points at a missing file: {src}")
                if installed in signed:
                    runtime = ctx["appRuntime"]
                    script = os.path.join(out, "libexec", "pkg", "cask-app.py")
                    with open(dest, "w") as f:
                        f.write("#!/bin/sh\nexec " + " ".join(shlex.quote(v) for v in
                            (runtime["python"], script, "exec", installed, inner)) + ' "$@"\n')
                    os.chmod(dest, 0o555)
                else:
                    os.symlink(os.path.relpath(resolved, bindir), dest)
            else:
                path = locate(staging, src)
                if arch.endswith("-linux"):
                    elf_ok_for_linux(path, arch)
                shutil.copy2(path, dest)
                os.chmod(dest, 0o555)
        elif kind == "manpage":
            import re

            # section 1..8, optionally ONE compression suffix; the whole
            # filename and the compressed bytes are kept as-is (no
            # unpack/decompress/recompress), e.g. tool.1.gz -> man1/tool.1.gz
            m = re.search(r"\.([1-8])(?:\.(?:gz|bz2|xz|zst|Z))?\Z", tgt)
            if not m:
                die(
                    "manpage target must end in .N (N=1..8), optionally "
                    "with one compression suffix "
                    f"(.gz/.bz2/.xz/.zst/.Z): {tgt!r}"
                )
            d = os.path.join(out, "share", "man", f"man{m.group(1)}")
            os.makedirs(d, exist_ok=True)
            dest = os.path.join(d, tgt)
            if os.path.lexists(dest):
                die(f"duplicate {kind} target: {tgt}")
            shutil.copy2(locate(staging, a["source"]), dest)
        else:
            d = os.path.join(out, COMP_DIRS[kind])
            os.makedirs(d, exist_ok=True)
            dest = os.path.join(d, tgt)
            if os.path.lexists(dest):
                die(f"duplicate {kind} target: {tgt}")
            shutil.copy2(locate(staging, a["source"]), dest)
    pack_signed_apps(ctx, out, signed)
    audit_output(out)


def main():
    if len(sys.argv) < 2:
        die("usage: cask-plan.py unpack|install ...")
    if sys.argv[1] == "unpack":
        cmd_unpack(
            sys.argv[2],
            sys.argv[3],
            sys.argv[4],
            sys.argv[5] if len(sys.argv) > 5 else "",
            sys.argv[6] if len(sys.argv) > 6 else "",
        )
    elif sys.argv[1] == "install":
        cmd_install(sys.argv[2], sys.argv[3], sys.argv[4])
    else:
        die(f"unknown subcommand {sys.argv[1]}")


if __name__ == "__main__":
    main()
