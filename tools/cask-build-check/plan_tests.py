#!/usr/bin/env python3
"""Focused regression tests for nix/casks/lib/plan.py (stdlib only).

Each case is a real defect fixed in review: pre-write boundary checks,
year-format bsdtar listings, UDIF content dispatch, HFS+ Mode/Folder link
records, nested raw-binary sources, exact nested locate, and the release
whitespace guard. Run: python3 tools/cask-build-check/plan_tests.py
"""
import os
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


def run_case(name):
    """Run fn in a child interpreter so die() exits cleanly."""
    code = (
        "import sys; sys.path.insert(0, %r); import plan; plan.%s"
        % (os.path.join(HERE, "..", "..", "nix", "casks", "lib"), name)
    )
    return subprocess.run([PY, "-c", code]).returncode == 0


def main():
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
            f.write('#!/bin/sh\necho application/x-tar\n')
        listing = ("-rw-r--r--  0 owner group 3 Dec 31  1982 dir/file.txt\n")
        with open(os.path.join(stub, "bsdtar"), "w") as f:
            f.write('#!/bin/sh\nprintf %s "' + listing.replace('\\', '\\\\') + '"\n')
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
    text = ("Path = Raycast/Applications\r\nFolder = -\nSize = 13\n"
            "Mode = lrwxr-xr-x\n\n"
            "Path = Raycast\r\nFolder = +\nMode = drwxr-xr-x\n\n")
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
    code = ("import sys;sys.path.insert(0,%r);import plan;"
            "plan.run=lambda c,**k:%r;"
            "plan._is_7z_family=lambda p:True;"
            "plan.sevenz_members('x')"
            % (os.path.join(HERE, "..", "..", "nix", "casks", "lib"), hard_text))
    if subprocess.run([PY, "-c", code]).returncode == 1:
        ok("7zz hard link member rejected")

    # 5. nested raw-binary source creates parent dirs
    with tempfile.TemporaryDirectory() as d:
        src = os.path.join(d, "s.bin")
        open(src, "wb").write(b"x")
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
            ["bash", "-c", f'b="$(find "{d}/bin" -type f | wc -l)"; '
             '[[ "${b//[[:space:]]/}" == "1" ]]'], ).returncode
        assert binaries == 0
        ok("release binary count trims wc -l padding")

    # 8. composed-link escapes (lexical per-link checks pass, graph fails)
    ex1 = [
        {"name": "a", "is_symlink": True, "is_dir": False, "link_target": "b/.."},
        {"name": "b", "is_symlink": True, "is_dir": False, "link_target": "."},
    ]
    ex2 = [
        {"name": "b", "is_symlink": True, "is_dir": False, "link_target": "."},
        {"name": "b/c", "is_symlink": True, "is_dir": False,
         "link_target": "../escape"},
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
                zi.external_attr = (0o120777 << 16)
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
        {"name": "Fw.framework/Versions", "is_symlink": False, "is_dir": True,
         "link_target": None},
        {"name": "Fw.framework/Versions/A", "is_symlink": False, "is_dir": True,
         "link_target": None},
        {"name": "Fw.framework/Versions/A/R", "is_symlink": False, "is_dir": False,
         "link_target": None},
        {"name": "Fw.framework/Versions/Current", "is_symlink": True,
         "is_dir": False, "link_target": "A"},
        {"name": "Fw.framework/R", "is_symlink": True, "is_dir": False,
         "link_target": "Versions/Current/R"},
    ]
    assert plan.validate_members(fw) == set()
    if not FAILED:
        ok("link cycle dies; framework Current chain resolves")
    wrap = [
        {"name": "Raycast/Applications", "is_symlink": True, "is_dir": False,
         "link_target": "/Applications"},
        {"name": "Raycast/Raycast.app/Contents/Applications", "is_symlink": True,
         "is_dir": False, "link_target": "/Applications"},
        {"name": "bin", "is_symlink": True, "is_dir": False,
         "link_target": "/usr/bin"},
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
            plan.cmd_unpack(src, os.path.join(d, "st"), "url",
                            "", "nested/dir/tool.gz")
        finally:
            plan._mime = old
            plan.zipfile.is_zipfile = old_iszip
        assert os.path.isfile(os.path.join(d, "st/nested/dir/tool"))
        ok("gzip fallback stages declared nested source with parents")

    # 10. shortcut needs the EXACT /Applications target
    bad_shortcut = [
        {"name": "Applications", "is_symlink": True, "is_dir": False,
         "link_target": "/etc"},
        {"name": "Vol/Applications", "is_symlink": True, "is_dir": False,
         "link_target": "/etc"},
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
                zi.external_attr = (0o120777 << 16)
        dest = os.path.join(d, "out")
        os.makedirs(dest)
        expect_die(plan.extract_zip, zpath, dest)
        if not FAILED and os.listdir(dest) == []:
            ok("'./'-prefixed composed escape dies before any payload write")
    cpio_like = [
        {"name": "./", "is_symlink": False, "is_dir": True, "link_target": None},
        {"name": "./", "is_symlink": False, "is_dir": True, "link_target": None},
        {"name": "./Applications", "is_symlink": False, "is_dir": True,
         "link_target": None},
        {"name": "./Applications/Foo.app", "is_symlink": False, "is_dir": True,
         "link_target": None},
        {"name": "./Applications/Foo.app/Contents/Info.plist",
         "is_symlink": False, "is_dir": False, "link_target": None},
    ]
    assert plan.validate_members(cpio_like) == set()
    if not FAILED:
        ok("cpio-style './Applications' bundle with root dir passes")

    if FAILED:
        for f in FAILED:
            print(f"FAIL: {f}")
        sys.exit(1)
    print("all plan.py regression checks passed")


if __name__ == "__main__":
    main()
