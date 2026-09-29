#!/usr/bin/env python3
"""Focused regression tests for nix/casks/lib/plan.py (stdlib only).

Each case is a real defect fixed in review: pre-write boundary checks,
year-format bsdtar listings, UDIF content dispatch, HFS+ Mode/Folder link
records, nested raw-binary sources, exact nested locate, and the release
whitespace guard. Run: python3 tools/cask-build-check/plan_tests.py
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "nix", "casks", "lib"))
import plan  # noqa: E402

PY = sys.executable
FAILED = []


def expect_die(fn, *args, **kw):
    try:
        fn(*args, **kw)
    except SystemExit:
        return
    FAILED.append(f"expected die: {fn.__name__}{args}")


def ok(name):
    print(f"PASS: {name}")


def put(path, data):
    with open(path, "w") as f:
        f.write(data)


def get(path):
    with open(path) as f:
        return f.read()


def run_case(name):
    """Run fn in a child interpreter so die() exits cleanly."""
    libdir = os.path.join(HERE, "..", "..", "nix", "casks", "lib")
    code = f"import sys; sys.path.insert(0, {libdir!r}); import plan; plan.{name}"
    # returncode is checked manually right below (check=False is explicit)
    return subprocess.run([PY, "-c", code], check=False).returncode == 0


def main():
    # prerequisites are MANDATORY: a missing tool is a loud failure,
    # never a hidden skip (the suite drives real bsdtar/7zz archives)
    missing = [t for t in ("bsdtar", "7zz") if shutil.which(t) is None]
    if missing:
        for t in missing:
            print(f"FAIL: prerequisite tool missing: {t}")
        sys.exit(1)

    # 1. pre-write boundary: escape through an existing symlink ancestor
    with tempfile.TemporaryDirectory() as d:
        esc = os.path.join(d, "esc")
        os.symlink("/tmp", esc)
        expect_die(plan._check_target_path, d, "esc/child")
        if not FAILED:
            ok("pre-write boundary rejects symlinked ancestor")

    # 2. bsdtar listing with year dates (mtime > 6 months old)
    with tempfile.TemporaryDirectory() as d:
        stub = os.path.join(d, "stub")
        os.makedirs(stub)
        with open(os.path.join(stub, "file"), "w") as f:
            f.write("#!/bin/sh\necho application/x-tar\n")
        listing = "-rw-r--r--  0 owner group 3 Dec 31  1982 dir/file.txt\n"
        with open(os.path.join(stub, "bsdtar"), "w") as f:
            f.write('#!/bin/sh\nprintf %s "' + listing.replace("\\", "\\\\") + '"\n')
        os.chmod(os.path.join(stub, "file"), 0o755)
        os.chmod(os.path.join(stub, "bsdtar"), 0o755)
        old_path = os.environ.get("PATH")
        os.environ["PATH"] = stub + os.pathsep + (old_path or "")
        try:
            members, omit = plan.bsdtar_members(os.path.join(d, "old.tar"))
        finally:
            if old_path:
                os.environ["PATH"] = old_path
            else:
                del os.environ["PATH"]
        assert members[0]["name"] == "dir/file.txt" and not omit
        ok("bsdtar year-format listing parses")

    # 3. UDIF content detection (koly trailer), not MIME/extension
    with tempfile.TemporaryDirectory() as d:
        dmg = os.path.join(d, "payload.dmg")
        with open(dmg, "wb") as f:
            f.write(b"zlib-stream-data" + b"\x00" * 508 + b"koly" + b"\x00" * 508)
        assert plan.is_udif(dmg)
        ok("UDIF koly trailer detected")

    # 4. HFS+ records: Mode=lrwx... without Symbolic Link, Folder=+ dirs,
    #    hard links rejected, CRLF stripped
    text = (
        "Path = Raycast/Applications\r\nFolder = -\nSize = 13\n"
        "Mode = lrwxr-xr-x\n\n"
        "Path = Raycast\r\nFolder = +\nMode = drwxr-xr-x\n\n"
    )
    hard_text = "Path = hard1\nMode = -rwxr-xr-x\nHard Link = hard0\n"
    with tempfile.NamedTemporaryFile("w", suffix=".7z", delete=False) as f:
        f.write(text)
        sevenz = f.name
    real = plan.run
    plan.run = lambda cmd, **kw: text
    recs = {m["name"]: m for m in plan.sevenz_members(sevenz)}
    plan.run = real
    assert recs["Raycast/Applications"]["is_symlink"] is True
    assert recs["Raycast/Applications"]["link_target"] is None
    assert recs["Raycast"]["is_dir"] is True
    ok("7zz Mode/Folder/CRLF records parse")
    # hard link rejection needs the listing; run in child for die()
    libdir = os.path.join(HERE, "..", "..", "nix", "casks", "lib")
    code = (
        f"import sys;sys.path.insert(0,{libdir!r});import plan;"
        f"plan.run=lambda c,**k:{hard_text!r};"
        "plan._is_7z_family=lambda p:True;"
        "plan.sevenz_members('x')"
    )
    if subprocess.run([PY, "-c", code], check=False).returncode == 1:
        ok("7zz hard link member rejected")

    # 4b. APFS-style records: `Symbolic Link = ` arrives EMPTY on ordinary
    #     dirs/files (Mode d.../-r... is authoritative); only a NONEMPTY
    #     value or mode=l is a link. mode=l with an empty value keeps
    #     link_target=None (HFS+ `-so` fallback read). An empty
    #     `Hard Link = ` is likewise NOT a hard link.
    apfs_text = (
        "Path = A.app\nMode = drwxr-xr-x\nSymbolic Link = \n\n"
        "Path = A.app/Contents/Versions\nMode = drwxr-xr-x\nSymbolic Link = \n\n"
        "Path = A.app/Contents/Current\nMode = lrwxr-xr-x\nSymbolic Link = Versions/A\n\n"
        "Path = A.app/Contents/lnk\nMode = lrwxr-xr-x\nSymbolic Link = \n\n"
        "Path = A.app/Contents/f\nMode = -rw-r--r--\nSymbolic Link = \nHard Link = \n\n"
    )
    with tempfile.NamedTemporaryFile("w", suffix=".7z", delete=False) as f:
        f.write(apfs_text)
        apfs7z = f.name
    plan.run = lambda cmd, **kw: apfs_text
    recs = {m["name"]: m for m in plan.sevenz_members(apfs7z)}
    plan.run = real
    apfs_ok = (
        recs["A.app"]["is_dir"]
        and not recs["A.app"]["is_symlink"]
        and recs["A.app/Contents/Versions"]["is_dir"]
        and not recs["A.app/Contents/Versions"]["is_symlink"]
        and recs["A.app/Contents/f"]["is_symlink"] is False
        and recs["A.app/Contents/Current"]["is_symlink"]
        and recs["A.app/Contents/Current"]["link_target"] == "Versions/A"
        and recs["A.app/Contents/lnk"]["is_symlink"]
        and recs["A.app/Contents/lnk"]["link_target"] is None
    )
    if apfs_ok:
        ok("7zz APFS empty Symbolic Link fields handled by Mode")
    else:
        FAILED.append("7zz APFS empty Symbolic Link misparsed")
    # an ACTUAL link whose `-so` read returns empty bytes fails closed
    code = (
        f"import sys;sys.path.insert(0,{libdir!r});import plan;"
        "plan.run=lambda c,**k:'';"
        "plan._7z_link_target('x','lnk')"
    )
    if subprocess.run([PY, "-c", code], check=False).returncode == 1:
        ok("7zz empty actual link target rejected")
    else:
        FAILED.append("empty actual link target not rejected")

    # 5. nested raw-binary source creates parent dirs
    with tempfile.TemporaryDirectory() as d:
        src = os.path.join(d, "s.bin")
        with open(src, "wb") as f:
            f.write(b"x")
        staging = os.path.join(d, "st")
        plan.cmd_unpack(src, staging, "url", "nested/dir/tool")
        assert os.path.isfile(os.path.join(staging, "nested/dir/tool"))
        ok("nested raw-binary source staged with parents")

    # 6. exact nested locate: unique hit resolves, ambiguity dies
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "V/W/nested"))
        open(os.path.join(d, "V/W/nested/tool"), "w").close()
        assert plan.locate(d, "nested/tool").endswith("V/W/nested/tool")
        ok("nested locate finds unique volume-nested artifact")
        os.makedirs(os.path.join(d, "V2/nested"))
        open(os.path.join(d, "V2/nested/tool"), "w").close()
        expect_die(plan.locate, d, "nested/tool")
        if not FAILED:
            ok("ambiguous nested locate dies")

    # 7. release whitespace guard: wc -l output with padding
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "bin"))
        open(os.path.join(d, "bin/pkg"), "w").close()
        binaries = subprocess.run(
            [
                "bash",
                "-c",
                f'b="$(find "{d}/bin" -type f | wc -l)"; '
                '[[ "${b//[[:space:]]/}" == "1" ]]',
            ],
            check=False,
        ).returncode
        assert binaries == 0
        ok("release binary count trims wc -l padding")

    # 8. composed-link escapes (lexical per-link checks pass, graph fails)
    ex1 = [
        {"name": "a", "is_symlink": True, "is_dir": False, "link_target": "b/.."},
        {"name": "b", "is_symlink": True, "is_dir": False, "link_target": "."},
    ]
    ex2 = [
        {"name": "b", "is_symlink": True, "is_dir": False, "link_target": "."},
        {
            "name": "b/c",
            "is_symlink": True,
            "is_dir": False,
            "link_target": "../escape",
        },
    ]
    expect_die(plan.validate_members, ex1)
    expect_die(plan.validate_members, ex2)
    if not FAILED:
        ok("composed link escapes die at validation (a->b/.., b->. and b/c)")
    # no payload write happens for a composed escape in a real zip
    import zipfile as zf

    with tempfile.TemporaryDirectory() as d:
        zpath = os.path.join(d, "evil.zip")
        with zf.ZipFile(zpath, "w") as z:
            z.writestr("b", ".")
            z.writestr("b/c", "../escape")
            for zi in z.infolist():  # mark members as symlinks
                zi.create_system = 3
                zi.external_attr = 0o120777 << 16
        dest = os.path.join(d, "out")
        os.makedirs(dest)
        expect_die(plan.extract_zip, zpath, dest)
        if not FAILED and os.listdir(dest) == []:
            ok("composed-escape zip dies before any payload write")
    # cycle dies; legitimate framework chain passes
    cycle = [
        {"name": "x", "is_symlink": True, "is_dir": False, "link_target": "y"},
        {"name": "y", "is_symlink": True, "is_dir": False, "link_target": "x"},
    ]
    expect_die(plan.validate_members, cycle)
    fw = [
        {
            "name": "Fw.framework/Versions",
            "is_symlink": False,
            "is_dir": True,
            "link_target": None,
        },
        {
            "name": "Fw.framework/Versions/A",
            "is_symlink": False,
            "is_dir": True,
            "link_target": None,
        },
        {
            "name": "Fw.framework/Versions/A/R",
            "is_symlink": False,
            "is_dir": False,
            "link_target": None,
        },
        {
            "name": "Fw.framework/Versions/Current",
            "is_symlink": True,
            "is_dir": False,
            "link_target": "A",
        },
        {
            "name": "Fw.framework/R",
            "is_symlink": True,
            "is_dir": False,
            "link_target": "Versions/Current/R",
        },
    ]
    assert plan.validate_members(fw) == set()
    if not FAILED:
        ok("link cycle dies; framework Current chain resolves")
    wrap = [
        {
            "name": "Raycast/Applications",
            "is_symlink": True,
            "is_dir": False,
            "link_target": "/Applications",
        },
        {
            "name": "Raycast/Raycast.app/Contents/Applications",
            "is_symlink": True,
            "is_dir": False,
            "link_target": "/Applications",
        },
        {"name": "bin", "is_symlink": True, "is_dir": False, "link_target": "/usr/bin"},
    ]
    omit = plan.validate_members(wrap[:1])
    assert omit == {"Raycast/Applications"}
    ok("volume-wrapper Applications shortcut omitted")
    expect_die(plan.validate_members, wrap[1:2])
    expect_die(plan.validate_members, wrap[2:])
    if not FAILED:
        ok("non-shortcut absolute links rejected inside .app and elsewhere")

    # 9. gzip fallback keeps declared nested source path
    import gzip as gz

    with tempfile.TemporaryDirectory() as d:
        src = os.path.join(d, "s.gz")
        with gz.open(src, "wb") as f:
            f.write(b"x")
        old = plan._mime
        plan._mime = lambda p: "application/gzip"
        old_iszip = plan.zipfile.is_zipfile
        plan.zipfile.is_zipfile = lambda p: False
        try:
            plan.cmd_unpack(src, os.path.join(d, "st"), "url", "", "nested/dir/tool.gz")
        finally:
            plan._mime = old
            plan.zipfile.is_zipfile = old_iszip
        assert os.path.isfile(os.path.join(d, "st/nested/dir/tool"))
        ok("gzip fallback stages declared nested source with parents")

    # 10. shortcut needs the EXACT /Applications target
    bad_shortcut = [
        {
            "name": "Applications",
            "is_symlink": True,
            "is_dir": False,
            "link_target": "/etc",
        },
        {
            "name": "Vol/Applications",
            "is_symlink": True,
            "is_dir": False,
            "link_target": "/etc",
        },
    ]
    expect_die(plan.validate_members, bad_shortcut[:1])
    expect_die(plan.validate_members, bad_shortcut[1:])
    if not FAILED:
        ok("Applications link with wrong absolute target rejected")

    # 11. '.'-prefixed members join the same graph: composed escape dies
    #     before any write; root '.' dir and cpio './Applications' pass
    prefixed = [
        {"name": "./a", "is_symlink": True, "is_dir": False, "link_target": "./b/.."},
        {"name": "./b", "is_symlink": True, "is_dir": False, "link_target": "."},
    ]
    expect_die(plan.validate_members, prefixed)
    with tempfile.TemporaryDirectory() as d:
        zpath = os.path.join(d, "prefixed.zip")
        with zf.ZipFile(zpath, "w") as z:
            z.writestr("./b", ".")
            z.writestr("./b/c", "../escape")
            for zi in z.infolist():
                zi.create_system = 3
                zi.external_attr = 0o120777 << 16
        dest = os.path.join(d, "out")
        os.makedirs(dest)
        expect_die(plan.extract_zip, zpath, dest)
        if not FAILED and os.listdir(dest) == []:
            ok("'./'-prefixed composed escape dies before any payload write")
    cpio_like = [
        {"name": "./", "is_symlink": False, "is_dir": True, "link_target": None},
        {"name": "./", "is_symlink": False, "is_dir": True, "link_target": None},
        {
            "name": "./Applications",
            "is_symlink": False,
            "is_dir": True,
            "link_target": None,
        },
        {
            "name": "./Applications/Foo.app",
            "is_symlink": False,
            "is_dir": True,
            "link_target": None,
        },
        {
            "name": "./Applications/Foo.app/Contents/Info.plist",
            "is_symlink": False,
            "is_dir": False,
            "link_target": None,
        },
    ]
    assert plan.validate_members(cpio_like) == set()
    if not FAILED:
        ok("cpio-style './Applications' bundle with root dir passes")

    # 12. zip link targets are bounded payloads: an oversized "target"
    #     is a decompression bomb read into RAM; control characters mean a
    #     corrupt link. Both fail closed before any write.
    with tempfile.TemporaryDirectory() as d:
        for label, data in (("bomb", b"a/" * 5000), ("ctl", b"ok\n../../tmp")):
            zpath = os.path.join(d, f"{label}.zip")
            with plan.zipfile.ZipFile(zpath, "w") as z:
                zi = plan.zipfile.ZipInfo("link")
                zi.create_system = 3
                zi.external_attr = 0o120777 << 16
                z.writestr(zi, data)
            dest = os.path.join(d, f"out-{label}")
            os.makedirs(dest)
            expect_die(plan.extract_zip, zpath, dest)
            if FAILED or os.listdir(dest) != []:
                FAILED.append(f"zip link target {label} wrote payload")
        if not FAILED:
            ok("oversized and control-char zip link targets die before write")

    # 13. tar fifo/device members and ALL hard links die at validation,
    #     before any write. Hard links fail closed, same policy as 7zz:
    #     in-tree, composed (target through symlink members), cycle, and
    #     dangling shapes are one unsupported class; the old lexical
    #     '..' check let composed targets pass and died mid-extract.
    import io
    import tarfile

    def tar_with(path, entries):
        with tarfile.open(path, "w") as t:
            for name, kind, link in entries:
                ti = tarfile.TarInfo(name)
                if kind == "file":
                    ti.size = 1
                    t.addfile(ti, io.BytesIO(b"x"))
                elif kind == "sym":
                    ti.type = tarfile.SYMTYPE
                    ti.linkname = link
                    t.addfile(ti)
                elif kind == "fifo":
                    ti.type = tarfile.FIFOTYPE
                    t.addfile(ti)
                elif kind == "link":
                    ti.type = tarfile.LNKTYPE
                    ti.linkname = link
                    t.addfile(ti)

    for label, entries in (
        ("fifo", [("f", "file", None), ("p", "fifo", None)]),
        ("in-tree-link", [("f", "file", None), ("h", "link", "f")]),
        (
            "composed-link",
            [
                ("a", "sym", "b"),
                ("b", "sym", "."),
                ("f", "file", None),
                ("h", "link", "a/../f"),
            ],
        ),
        ("cycle-link", [("h1", "link", "h2"), ("h2", "link", "h1")]),
        ("dangling-link", [("h", "link", "missing")]),
    ):
        with tempfile.TemporaryDirectory() as d:
            tpath = os.path.join(d, f"{label}.tar")
            tar_with(tpath, entries)
            dest = os.path.join(d, "out")
            os.makedirs(dest)
            expect_die(plan.extract_tar, tpath, dest)
            if FAILED or os.listdir(dest) != []:
                FAILED.append(f"tar {label} left partial extraction")
        if not FAILED:
            ok(f"tar {label} hard/special member dies before any write")

    # 14. colliding completion/manpage targets die like app/binary targets
    #     (they used to be silently overwritten by the later artifact)
    with tempfile.TemporaryDirectory() as d:
        staging = os.path.join(d, "st")
        os.makedirs(os.path.join(staging, "d"))
        for n in ("z1", "z2", "m1", "m2"):
            put(os.path.join(staging, "d", n), n.upper())
        ctx = {
            "plan": {
                "artifacts": [
                    {"kind": "zsh-completion", "source": "d/z1", "target": "tool"},
                    {"kind": "zsh-completion", "source": "d/z2", "target": "tool"},
                    {"kind": "manpage", "source": "d/m1", "target": "tool.1"},
                    {"kind": "manpage", "source": "d/m2", "target": "tool.1"},
                ]
            },
            "baseline": "10.14",
            "system": "aarch64-darwin",
            "token": "t",
        }
        pj = os.path.join(d, "plan.json")
        put(pj, json.dumps(ctx))
        out = os.path.join(d, "out")
        os.makedirs(out)
        expect_die(plan.cmd_install, pj, staging, out)
        kept = os.path.join(out, "share/zsh/site-functions/tool")
        if not FAILED and get(kept) == "Z1":
            ok("duplicate completion/manpage targets die; first artifact stays")

    # 15. real xar pkg end-to-end: ordinary relocatable payload installs;
    #     Scripts/Distribution/Plugins/nested pkg components are refused
    import gzip

    def build_pkg(w, comp_extra=None):
        comp = os.path.join(w, "p", "com.example.comp")
        os.makedirs(comp, exist_ok=True)
        tree = os.path.join(w, "tree", "A.app", "Contents")
        os.makedirs(tree, exist_ok=True)
        put(
            os.path.join(tree, "Info.plist"),
            '<?xml version="1.0"?><plist version="1.0"><dict>'
            "<key>CFBundleName</key><string>A</string></dict></plist>",
        )
        cpio = os.path.join(comp, "Payload")
        subprocess.run(
            [
                "bsdtar",
                "--format",
                "cpio",
                "-cf",
                cpio + ".raw",
                "-C",
                os.path.join(w, "tree"),
                ".",
            ],
            check=True,
            capture_output=True,
        )
        with open(cpio + ".raw", "rb") as fi, gzip.open(cpio, "wb") as fo:
            shutil.copyfileobj(fi, fo)
        os.remove(cpio + ".raw")
        if comp_extra:
            comp_extra(comp)
        xar = os.path.join(w, "p.xar")
        subprocess.run(
            [
                "bsdtar",
                "--format",
                "xar",
                "-cf",
                xar,
                "-C",
                os.path.join(w, "p"),
                "com.example.comp",
            ],
            check=True,
            capture_output=True,
        )
        return xar

    def pkg_run(xar):
        w = os.path.dirname(xar)
        staging = os.path.join(w, "st")
        out = os.path.join(w, "out")
        os.makedirs(staging, exist_ok=True)
        os.makedirs(out, exist_ok=True)
        pj = os.path.join(w, "plan.json")
        put(
            pj,
            json.dumps(
                {
                    "plan": {
                        "artifacts": [
                            {"kind": "pkg", "source": "p.xar", "target": None}
                        ]
                    },
                    "baseline": "10.14",
                    "system": "aarch64-darwin",
                    "token": "t",
                }
            ),
        )
        code = (
            f"import sys;sys.path.insert(0,{libdir!r});import plan;"
            f"plan.cmd_unpack({xar!r},{staging!r},'p.xar');"
            f"plan.cmd_install({pj!r},{staging!r},{out!r})"
        )
        p = subprocess.run(
            [PY, "-c", code], capture_output=True, text=True, check=False
        )
        return p, out

    libdir = os.path.join(HERE, "..", "..", "nix", "casks", "lib")
    with tempfile.TemporaryDirectory() as d:
        p, out = pkg_run(build_pkg(d))
        plist = os.path.join(out, "Applications/A.app/Contents/Info.plist")
        if p.returncode == 0 and os.path.isfile(plist):
            ok("real xar pkg: ordinary relocatable payload installs")
        else:
            FAILED.append(f"clean pkg failed: {p.stderr.strip()[:120]}")
    for label, extra, pattern in (
        (
            "scripts",
            lambda c: put(os.path.join(c, "Scripts"), "#!"),
            "installer scripts",
        ),
        (
            "distribution",
            lambda c: put(os.path.join(c, "Distribution"), "<s/>"),
            "Distribution installer logic",
        ),
        (
            "plugins",
            lambda c: os.makedirs(os.path.join(c, "Plugins")),
            "installer plugins",
        ),
        (
            "nested",
            lambda c: os.makedirs(os.path.join(c, "inner.pkg")),
            "nests component",
        ),
    ):
        with tempfile.TemporaryDirectory() as d:
            p, out = pkg_run(build_pkg(d, extra))
            apps = os.path.join(out, "Applications")
            if p.returncode == 1 and pattern in p.stderr and not os.path.exists(apps):
                ok(f"pkg {label} component refused, no output bundle")
            else:
                FAILED.append(f"pkg {label} not refused: rc={p.returncode}")

    # 16. the omit comparison is canonical: real zip/tar archives store
    #     the DMG shortcut as './Applications' while validate_members
    #     normalizes it. Extractors compared raw spellings against the
    #     omit set, so the inert shortcut was refused (zip: "reject
    #     escaping vendor link"; tar: data-filter abort) and the good
    #     payload was lost. Payload must survive; a wrong absolute
    #     target must still die.
    def shortcut_zip(path, target):
        with plan.zipfile.ZipFile(path, "w") as z:
            zi = plan.zipfile.ZipInfo("./Applications")
            zi.create_system = 3
            zi.external_attr = 0o120777 << 16
            z.writestr(zi, target)
            z.writestr("Good.app/Contents/Info.plist", "PLIST")

    def shortcut_tar(path, target):
        with tarfile.open(path, "w") as t:
            ti = tarfile.TarInfo("./Applications")
            ti.type = tarfile.SYMTYPE
            ti.linkname = target
            t.addfile(ti)
            ti = tarfile.TarInfo("Good.app/Contents/Info.plist")
            ti.size = 5
            t.addfile(ti, io.BytesIO(b"PLIST"))

    extractors = {"zip": plan.extract_zip, "tar": plan.extract_tar}
    for fmt, build in (("zip", shortcut_zip), ("tar", shortcut_tar)):
        extract = extractors[fmt]
        with tempfile.TemporaryDirectory() as d:
            good = os.path.join(d, "good")
            build(good, "/Applications")
            dest = os.path.join(d, "out")
            os.makedirs(dest)
            extract(good, dest)
            plist = os.path.join(dest, "Good.app/Contents/Info.plist")
            shortcut = os.path.join(dest, "Applications")
            if not os.path.isfile(plist) or os.path.lexists(shortcut):
                FAILED.append(f"{fmt} './Applications' shortcut not omitted")
        with tempfile.TemporaryDirectory() as d:
            bad = os.path.join(d, "bad")
            build(bad, "/tmp")
            dest = os.path.join(d, "out")
            os.makedirs(dest)
            expect_die(extract, bad, dest)
            if FAILED or os.listdir(dest) != []:
                FAILED.append(f"{fmt} wrong-target shortcut not refused pre-write")
        if not FAILED:
            ok(
                f"{fmt} './Applications' shortcut omitted; payload and refusal both kept"
            )

    # 17. REAL 7zz extraction (record mocks do not cover it): a tiny
    #     archive built with 7zz's documented -snl switch (`7zz --help`:
    #     "-snl : store symbolic links as links") carries an ordinary
    #     nested file, a legit relative link, the absolute /Applications
    #     shortcut, and (second archive) a dangerous relative link. Real
    #     -snl records use `Attributes = A lrwxrwxrwx` with NO Mode or
    #     Symbolic Link field; before the fix those members passed as
    #     regular files, so 7zz itself wrote the links unvalidated.
    help_text = subprocess.run(
        ["7zz", "--help"], capture_output=True, text=True, check=False
    ).stdout
    if "-snl" not in help_text:
        FAILED.append("7zz does not document -snl")

    def snl_archive(path, extra=None):
        w = os.path.join(os.path.dirname(path), "tree-" + os.path.basename(path))
        os.makedirs(os.path.join(w, "Good.app/Contents"), exist_ok=True)
        os.makedirs(os.path.join(w, "Fw.framework/Versions/A"), exist_ok=True)
        put(os.path.join(w, "Good.app/Contents/Info.plist"), "PLIST")
        put(os.path.join(w, "Fw.framework/Versions/A/R"), "R")
        os.symlink("A", os.path.join(w, "Fw.framework/Versions/Current"))
        os.symlink("/Applications", os.path.join(w, "Applications"))
        if extra:
            extra(w)
        # cwd=: 7zz has no -C switch; archive the tree in place
        subprocess.run(
            ["7zz", "a", "-snl", os.path.abspath(path), "."],
            cwd=w,
            check=True,
            capture_output=True,
        )
        return path

    with tempfile.TemporaryDirectory() as d:
        arc = snl_archive(os.path.join(d, "good.7z"))
        recs = {m["name"]: m for m in plan.sevenz_members(arc)}
        links_ok = (
            recs["Applications"]["is_symlink"]
            and recs["Fw.framework/Versions/Current"]["is_symlink"]
        )
        dest = os.path.join(d, "out")
        os.makedirs(dest)
        plan.extract_7zz(arc, dest)
        cur = os.path.join(dest, "Fw.framework/Versions/Current")
        kept = (
            os.path.isfile(os.path.join(dest, "Good.app/Contents/Info.plist"))
            and os.path.islink(cur)
            and os.readlink(cur) == "A"
            and not os.path.lexists(os.path.join(dest, "Applications"))
        )
        if links_ok and kept:
            ok(
                "real 7zz -snl extract: file kept, relative link re-created, shortcut omitted"
            )
        else:
            FAILED.append("real 7zz -snl extract mishandled link members")
    with tempfile.TemporaryDirectory() as d:
        arc = snl_archive(
            os.path.join(d, "evil.7z"),
            lambda w: os.symlink("../../escape", os.path.join(w, "evil")),
        )
        dest = os.path.join(d, "out")
        os.makedirs(dest)
        expect_die(plan.extract_7zz, arc, dest)
        if FAILED or os.listdir(dest) != []:
            FAILED.append("7zz dangerous relative link wrote payload")
        if not FAILED:
            ok("real 7zz -snl dangerous relative link dies before any write")

    # 18. REAL native APFS DMG (committed binary fixture, parent-built on
    #     macOS 15.7.7 via `hdiutil create -srcfolder ... -format UDZO`;
    #     see fixtures/native-apfs.dmg.README.md). Before the fix every
    #     plain directory was misread as a dangling zero-length link and
    #     the unpack died with `dangling link in archive`. The fixture is
    #     MANDATORY: missing means FAIL, never a skip.
    dmg = os.path.join(HERE, "fixtures", "native-apfs.dmg")
    if not os.path.isfile(dmg):
        FAILED.append(
            "native APFS fixture missing: tools/cask-build-check/fixtures/native-apfs.dmg"
        )
    else:
        with tempfile.TemporaryDirectory() as d:
            st = os.path.join(d, "staging")
            plan.cmd_unpack(dmg, st, "download")
            cur = os.path.join(st, "Probe.app/Contents/Current")
            ver = os.path.join(st, "Probe.app/Contents/Versions")
            checks = {
                "payload file extracted": os.path.isfile(
                    os.path.join(st, "Probe.app/Contents/Info.plist")
                ),
                "dir not misread as link": os.path.isdir(ver)
                and not os.path.islink(ver),
                "internal link preserved": os.path.islink(cur)
                and os.readlink(cur) == "Versions/A",
                "internal link resolves in-tree": os.path.isfile(
                    os.path.join(os.path.realpath(cur), "probe")
                ),
                "absolute shortcut omitted": not os.path.lexists(
                    os.path.join(st, "Applications")
                ),
            }
            # renamed app + anchored absolute-path CLI run prints fixture-ok
            os.rename(os.path.join(st, "Probe.app"), os.path.join(st, "Renamed.app"))
            run_out = subprocess.run(
                [os.path.join(st, "Renamed.app/Contents/Versions/A/probe")],
                capture_output=True,
                text=True,
                check=False,
            )
            checks["renamed app CLI prints fixture-ok"] = (
                run_out.returncode == 0 and run_out.stdout.strip() == "fixture-ok"
            )
            bad = [k for k, v in checks.items() if not v]
            if bad:
                FAILED.append(f"native APFS DMG unpack wrong: {bad}")
            else:
                ok(
                    "native APFS DMG: dirs kept, internal link preserved, shortcut omitted, CLI prints fixture-ok"
                )

    # 19. manpage target sections 1..8 with an optional ONE recognized
    #     compression suffix (.gz/.bz2/.xz/.zst/.Z). The whole filename
    #     and the source compressed bytes are preserved (no unpacking);
    #     the section comes from the target. Real gzip/bz2/xz content is
    #     built with the stdlib. Invalid sections, a missing section, an
    #     unknown suffix, double compression, trailing junk, unsafe
    #     paths, and duplicate compressed targets all die.
    import bz2
    import gzip as gzlib
    import lzma

    def man_install(items, check=None):
        """items: (source relpath, bytes, target). Returns (died, ok)."""
        with tempfile.TemporaryDirectory() as d:
            staging = os.path.join(d, "st")
            os.makedirs(staging)
            arts = []
            for rel, data, tgt in items:
                p = os.path.join(staging, rel)
                os.makedirs(os.path.dirname(p), exist_ok=True)
                with open(p, "wb") as f:
                    f.write(data)
                arts.append({"kind": "manpage", "source": rel, "target": tgt})
            pj = os.path.join(d, "plan.json")
            put(
                pj,
                json.dumps(
                    {
                        "plan": {"artifacts": arts},
                        "baseline": "10.14",
                        "system": "aarch64-darwin",
                        "token": "t",
                    }
                ),
            )
            out = os.path.join(d, "out")
            os.makedirs(out)
            died = None
            try:
                plan.cmd_install(pj, staging, out)
            except SystemExit as e:
                # only a nonzero exit counts as rejection (die() exits 1)
                died = bool(getattr(e, "code", 1))
            # the output check ALWAYS runs, even after a rejection
            okflag = check(out) if check else True
            return died is True, okflag

    def read_bytes(path):
        with open(path, "rb") as f:
            return f.read()

    # plain sections 1..8 all install under share/man/man<N>
    items = [(f"d/m{s}", f"P{s}".encode(), f"tool.{s}") for s in range(1, 9)]

    def plain_check(out):
        return all(
            os.path.isfile(os.path.join(out, "share", f"man/man{s}", f"tool.{s}"))
            for s in range(1, 9)
        )

    if man_install(items, plain_check) == (False, True):
        ok("plain manpage sections 1..8 install under share/man/man<N>")
    else:
        FAILED.append("plain manpage sections 1..8 misrouted")

    # real gzip manpage named goreleaser.1.gz: byte-identical, man1
    raw = b"GORELEASER(1) manual page\n"
    gz_bytes = gzlib.compress(raw)
    died, okflag = man_install(
        [("d/g", gz_bytes, "goreleaser.1.gz")],
        lambda o: (
            read_bytes(os.path.join(o, "share/man/man1/goreleaser.1.gz")) == gz_bytes
            and gzlib.decompress(
                read_bytes(os.path.join(o, "share/man/man1/goreleaser.1.gz"))
            )
            == raw
        ),
    )
    if not died and okflag:
        ok("real gzip manpage goreleaser.1.gz installed byte-identical in man1")
    else:
        FAILED.append("goreleaser.1.gz manpage mishandled")

    # bz2/xz/zst/Z suffixes: compressed bytes copied untouched
    comp_cases = (
        ("d/b", bz2.compress(raw), "tool.1.bz2", "man1"),
        ("d/x", lzma.compress(raw), "tool.2.xz", "man2"),
        ("d/z", b"zst-payload", "tool.3.zst", "man3"),
        ("d/Z", b"Z-payload", "tool.4.Z", "man4"),
    )
    all_ok = True
    for rel, data, tgt, sub in comp_cases:
        died, okflag = man_install(
            [(rel, data, tgt)],
            lambda o, d=data, t=tgt, s=sub: (
                read_bytes(os.path.join(o, "share/man", s, t)) == d
            ),
        )
        all_ok = all_ok and not died and okflag
    if all_ok:
        ok("bz2/xz/zst/Z manpage suffixes copied byte-identical per section")
    else:
        FAILED.append("compressed manpage suffix routing wrong")

    # renamed compressed target: section comes from the TARGET (.7.gz)
    died, okflag = man_install(
        [("d/g", gz_bytes, "tool.7.gz")],
        lambda o: read_bytes(os.path.join(o, "share/man/man7/tool.7.gz")) == gz_bytes,
    )
    if not died and okflag:
        ok("renamed compressed manpage target tool.7.gz routed to man7")
    else:
        FAILED.append("renamed compressed manpage target misrouted")

    # duplicate compressed target: second dies, first stays (this plan
    # has ONLY manpage artifacts, so the duplicate branch is reached)
    first_gz = gzlib.compress(b"FIRST")
    second_gz = gzlib.compress(b"SECOND")
    died, okflag = man_install(
        [
            ("d/m1", first_gz, "tool.1.gz"),
            ("d/m2", second_gz, "tool.1.gz"),
        ],
        lambda o: read_bytes(os.path.join(o, "share/man/man1/tool.1.gz")) == first_gz,
    )
    if died and okflag:
        ok("duplicate compressed manpage target dies; first artifact stays")
    else:
        FAILED.append("duplicate compressed manpage target not refused")

    # invalid targets die with nothing written
    for tgt in (
        "tool.0",
        "tool.9",
        "tool.gz",
        "tool.1.lzma",
        "tool.1.gz.gz",
        "tool.1.gz.bak",
        "tool.1.gz\n",
        "tool.1.gz\r",
        "share/man/tool.1",
    ):
        died, okflag = man_install(
            [("d/m", b"X", tgt)],
            lambda o: (
                not os.path.exists(os.path.join(o, "share"))
                or os.listdir(os.path.join(o, "share")) == []
            ),
        )
        if died and okflag:
            ok(f"invalid manpage target refused: {tgt!r}")
        else:
            FAILED.append(f"invalid manpage target accepted: {tgt!r}")

    if FAILED:
        for f in FAILED:
            print(f"FAIL: {f}")
        sys.exit(1)
    print("all plan.py regression checks passed")


if __name__ == "__main__":
    main()
